# Console-facing, read-only views of a grantable secret of any kind: where its
# value is resolved from (backend + reference), what it injects into, the
# per-kind configuration rows, and whether an OAuth integration manages it.
#
# Never includes a secret value: an inline (control_plane) source is shown as
# the literal reference "inline".
module SecretConsoleView
  SOURCE_TYPE_LABELS = {
    "env" => "Env", "aws_sm" => "AWS-SM", "aws_ssm" => "AWS-SSM",
    "1password" => "1Password", "1password_connect" => "1Password-Connect",
    "control_plane" => "Inline", "token_broker" => "Token-Broker",
    "workload_identity" => "Workload-Identity"
  }.freeze

  module_function

  def source_type_label(type)
    SOURCE_TYPE_LABELS[type] || type
  end

  # Where a secret's value is resolved from: [{ role:, type:, type_label:, ref: }].
  # role and ref may be nil. Empty when the secret has no source.
  def source_segments(record)
    raw_segments(record).map { |seg| seg.merge(type_label: source_type_label(seg[:type])) }
  end

  def source_types(record)
    raw_segments(record).map { |seg| source_type_label(seg[:type]) }.uniq
  end

  # { broker_credential_id:, oauth_app_id:, oauth_app_slug: } when a static
  # secret wraps a broker credential minted by an OAuth flow, else nil.
  def managed_by(record)
    credential = record.try(:broker_credential)
    return nil unless credential

    {
      broker_credential_id: credential.oid,
      oauth_app_id: credential.oauth_app&.oid,
      oauth_app_slug: credential.oauth_app&.slug
    }
  end

  # The request header (or other target) the resolved secret is injected into,
  # or nil when it isn't request-header injected (e.g. a Postgres DSN).
  def injection(record)
    case record
    when StaticSecret then static_injection(record)
    when GcpAuthSecret then "Authorization: Bearer"
    when GcpIdTokenSecret then "#{record.header.presence || "authorization"}: Bearer"
    when AwsAuthSecret then "Authorization: AWS4-HMAC-SHA256"
    when OauthTokenSecret
      header = record.header.presence || "Authorization"
      prefix = record.value_prefix.presence&.strip
      prefix ? "#{header}: #{prefix} …" : header
    when HmacSecret
      names = Array(record.headers).filter_map { |h| h["name"].presence if h.is_a?(Hash) }
      names.presence&.join(", ")
    end
  end

  def configuration(record)
    rows = []
    if (inj = injection(record)).present?
      rows << [ "Injects into", inj ]
    end
    rows.concat(kind_configuration(record))
    rows.map { |label, value| { label: label, value: value.to_s } }
  end

  def signed_headers(record)
    return nil unless record.is_a?(HmacSecret)

    Array(record.headers).filter_map do |h|
      { name: h["name"], value: h["value"] } if h.is_a?(Hash)
    end
  end

  def rules(record)
    return nil unless record.respond_to?(:rules)

    record.rules.map do |r|
      { host: r.host, cidr: r.cidr, position: r.position, http_methods: r.http_methods, paths: r.paths }
    end
  end

  # A pg_dsn session setting as { name:, kind:, value: } where kind is
  # "literal" or the value_from reference key.
  def pg_setting_row(setting)
    s = setting.is_a?(Hash) ? setting.deep_stringify_keys : {}
    ref = s["value_from"]
    kind = (PgDsnSecret::VALUE_FROM_KEYS & ref.keys.map(&:to_s)).first if ref.is_a?(Hash)
    return { name: s["name"].to_s, kind: kind, value: ref[kind].to_s } if kind

    { name: s["name"].to_s, kind: "literal", value: s["value"].to_s }
  end

  def kind_configuration(record)
    case record
    when StaticSecret
      [ [ "Credential profile", record.kind ], [ "Mode", record.replace_config.present? ? "replace" : "inject" ] ]
    when GcpAuthSecret
      rows = []
      rows << [ "Subject", record.subject ] if record.subject.present?
      rows << [ "Scopes", Array(record.scopes).join("\n") ] if record.scopes.present?
      rows
    when GcpIdTokenSecret
      [ [ "Audience", record.audience ], [ "Header", record.header.presence || "authorization" ] ]
    when AwsAuthSecret
      rows = []
      rows << [ "Allowed regions", Array(record.allowed_regions).join("\n") ] if record.allowed_regions.present?
      rows << [ "Allowed services", Array(record.allowed_services).join("\n") ] if record.allowed_services.present?
      rows
    when OauthTokenSecret
      rows = [ [ "Grant", record.grant ], [ "Token endpoint", record.token_endpoint ] ]
      rows << [ "Audience", record.audience ] if record.audience.present?
      rows << [ "Scopes", Array(record.scopes).join("\n") ] if record.scopes.present?
      rows
    when PgDsnSecret
      rows = []
      rows << [ "Database", record.database ] if record.database.present?
      rows << [ "Role", record.role ] if record.role.present?
      if record.settings.present?
        rows << [ "Session settings", record.settings.map { |s|
          row = pg_setting_row(s)
          value = row[:kind] == "literal" ? row[:value] : "#{row[:kind]}(#{row[:value]})"
          "#{row[:name]} = #{value}"
        }.join("\n") ]
      end
      rows
    when HmacSecret
      [
        [ "Timestamp format", record.timestamp_format ],
        [ "Algorithm", record.signature_algorithm ],
        [ "Key encoding", record.signature_key_encoding ],
        [ "Output encoding", record.signature_output_encoding ],
        [ "Message", record.signature_message ]
      ]
    else
      []
    end
  end

  def raw_segments(record)
    case record
    when StaticSecret then [ source_segment(record.source) ].compact
    when PgDsnSecret then [ source_segment(record.dsn_source) ].compact
    when GcpIdTokenSecret then [ source_segment(record.keyfile_source) ].compact
    when SshKeySecret then [ source_segment(record.key_source) ].compact
    when GcpAuthSecret
      if record.keyfile_source
        [ source_segment(record.keyfile_source) ].compact
      elsif record.credentials_provider.is_a?(Hash) && record.credentials_provider["type"].present?
        [ { role: nil, type: record.credentials_provider["type"], ref: nil } ]
      else
        []
      end
    when OauthTokenSecret
      record.sources.select(&:credential_field?).sort_by(&:role).filter_map { |s| source_segment(s, role: s.role) }
    when HmacSecret, AwsAuthSecret
      record.sources.sort_by(&:role).filter_map { |s| source_segment(s, role: s.role) }
    else
      []
    end
  end

  def source_segment(source, role: nil)
    return nil unless source

    key = SecretKinds::SOURCE_REF_KEYS[source.source_type]
    ref =
      if key && source.config.is_a?(Hash)
        source.config[key]
      elsif source.source_type == "control_plane"
        "inline"
      end

    { role: role, type: source.source_type, ref: ref.presence }
  end

  def static_injection(record)
    if record.inject_config.present?
      cfg = record.inject_config
      return cfg["header"] if cfg["header"].present?
      return "?#{cfg["query_param"]}" if cfg["query_param"].present?
    elsif record.replace_config.present?
      headers = Array(record.replace_config["match_headers"])
      return headers.join(", ") if headers.any?
    end
    nil
  end
end
