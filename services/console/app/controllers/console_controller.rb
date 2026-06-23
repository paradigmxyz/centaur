# Operator console: a lightweight, server-rendered HTML view over principals,
# their effective grants, and secrets. Read-only; gated behind a console session
# via ApplicationController#require_login. Distinct from the JSON API.
class ConsoleController < ApplicationController
  include SecretKinds

  layout "console"

  # Friendly labels for the source backend (and the gcp_auth credentials_provider
  # type). The secrets table shows only this -- the full reference lives on the
  # secret detail page.
  SOURCE_TYPE_LABELS = {
    "env" => "Env", "aws_sm" => "AWS-SM", "aws_ssm" => "AWS-SSM",
    "1password" => "1Password", "1password_connect" => "1Password-Connect",
    "control_plane" => "Inline", "token_broker" => "Token-Broker",
    "workload_identity" => "Workload-Identity"
  }.freeze

  def principals
    @principals = Principal.order(created_at: :asc, id: :asc)
  end

  def principal
    @principal = Principal.find_by_oid!(params[:id])
    @roles = @principal.roles.order(:id)
    @granted = {
      "static" => @principal.granted_static_secrets,
      "gcp_auth" => @principal.granted_gcp_auth_secrets,
      "gcp_id_token" => @principal.granted_gcp_id_token_secrets,
      "aws_auth" => @principal.granted_aws_auth_secrets,
      "oauth_token" => @principal.granted_oauth_token_secrets,
      "pg_dsn" => @principal.granted_pg_dsn_secrets,
      "hmac" => @principal.granted_hmac_secrets
    }
    # Direct grants (revocable here) -- distinct from @granted, which also folds in
    # grants inherited from roles.
    @direct_grants = @principal.grants
      .includes(Grant::GRANTABLE_ASSOCIATIONS)
      .order(:id)
    # How each effective secret is reached, for the Source column: keyed by
    # [kind, secret_id], a list of { type: :direct } / { type: :role, role: } --
    # a secret can be granted directly and/or through one or more roles.
    @grant_sources = Hash.new { |h, k| h[k] = [] }
    @principal.effective_grants.includes(:role).each do |grant|
      assoc = Grant::GRANTABLE_ASSOCIATIONS.find { |a| grant.public_send("#{a}_id") }
      next unless assoc
      kind = assoc.to_s.delete_suffix("_secret")
      @grant_sources[[ kind, grant.public_send("#{assoc}_id") ]] <<
        (grant.role ? { type: :role, role: grant.role } : { type: :direct })
    end
    # Assignment options for the inline forms. Roles are namespace-scoped, so
    # only same-namespace roles are assignable from this principal. Secrets span
    # all namespaces; the namespace is shown as a label on each option.
    # Already-directly-granted secrets are filtered out of the grant dropdown
    # (they're in the table above); a role-inherited secret stays offered, so it
    # can be promoted to a direct grant.
    @assignable_roles = Role
      .where(namespace: @principal.namespace)
      .where.not(id: @principal.role_ids)
      .order(:id)
    granted_ids = Hash.new { |h, k| h[k] = [] }
    @direct_grants.each do |grant|
      assoc = Grant::GRANTABLE_ASSOCIATIONS.find { |a| grant.public_send("#{a}_id") }
      next unless assoc
      granted_ids[assoc.to_s.delete_suffix("_secret")] << grant.public_send("#{assoc}_id")
    end
    @assignable_secrets = SECRET_KINDS.each_with_object({}) do |(kind, cfg), acc|
      acc[kind] = cfg[:model].where.not(id: granted_ids[kind]).order(:namespace, :id)
    end
  end

  def secrets
    @secrets_by_kind = SECRET_KINDS.transform_values do |cfg|
      rel = cfg[:model].includes(cfg[:includes]).order(created_at: :asc, id: :asc)
      # Static secrets may wrap a broker credential (the "managed" badge); eager
      # load the credential and its app so the list doesn't fan out per row.
      rel = rel.includes(broker_credential: :oauth_app) if cfg[:model] == StaticSecret
      rel
    end
  end

  def secret
    cfg = SECRET_KINDS[params[:kind]]
    return render plain: "secret not found", status: :not_found unless cfg

    @kind = params[:kind]
    @secret = cfg[:model].includes(cfg[:includes]).find_by_oid!(params[:id])
    grantable = @secret.class.name.underscore.to_sym
    @role_grants = Grant.where(grantable => @secret).where.not(role_id: nil).includes(:role).order(:id)
    granted_role_ids = @role_grants.map(&:role_id)
    @assignable_roles = Role
      .where(namespace: @secret.namespace)
      .where.not(id: granted_role_ids)
      .order(:id)
  end

  # Managed broker credentials and their refresh-loop status. Distinct from
  # SECRET_KINDS because a broker credential is not grantable -- it is referenced
  # by a token_broker source rather than granted directly.
  def credentials
    @credentials = BrokerCredential.includes(:oauth_app).order(created_at: :asc, id: :asc)
  end

  def credential
    @credential = BrokerCredential.find_by_oid!(params[:id])
    # The grantable static secret wrapping this credential, for the cross-link.
    @wrapping_secret = @credential.static_secret
  end

  # Registered OAuth apps and the consent flows they drive. Like credentials,
  # an app is not grantable -- it is the durable config behind the public
  # /oauth/:provider/start flow.
  def oauth_apps
    @oauth_apps = OauthApp.order(created_at: :asc, id: :asc)
    # One count query for the whole table rather than one per row.
    @minted_counts = BrokerCredential.group(:oauth_app_id).count
  end

  def oauth_app
    @oauth_app = OauthApp.find_by_oid!(params[:id])
    @minted_credentials = @oauth_app.broker_credentials.order(created_at: :asc, id: :asc)
  end

  # Where a secret's value is resolved from, as a list of segments. Each segment
  # is a hash { role:, type:, ref: } where +type+ is the source backend
  # (e.g. "env", "aws_sm") and +ref+ is the reference within it (e.g. "STRIPE_KEY",
  # "prod/db"); both +role+ and +ref+ may be nil. Empty when the secret has no source.
  helper_method :secret_source_segments
  def secret_source_segments(record)
    case record
    when StaticSecret then [ source_segment(record.source) ].compact
    when PgDsnSecret  then [ source_segment(record.dsn_source) ].compact
    when GcpIdTokenSecret then [ source_segment(record.keyfile_source) ].compact
    when GcpAuthSecret
      if record.keyfile_source
        [ source_segment(record.keyfile_source) ].compact
      elsif (type = provider_label(record.credentials_provider))
        [ { role: nil, type: type, ref: nil } ]
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

  # The distinct source backends a secret resolves from, as friendly labels
  # (e.g. ["1Password", "Env"]). Used by the secrets table, which shows only the
  # backend -- not the reference. Empty when the secret has no source.
  helper_method :secret_source_types
  def secret_source_types(record)
    secret_source_segments(record).map { |seg| source_type_label(seg[:type]) }.uniq
  end

  # Friendly label for a source backend / provider type, falling back to the raw
  # value for anything not in the table.
  helper_method :source_type_label
  def source_type_label(type)
    SOURCE_TYPE_LABELS[type] || type
  end

  # The request header (or other target) the resolved secret is injected into.
  # nil when the secret isn't request-header injected (e.g. a Postgres DSN,
  # matched by listener port rather than by request).
  helper_method :secret_injection
  def secret_injection(record)
    case record
    when StaticSecret  then static_injection(record)
    when GcpAuthSecret  then "Authorization: Bearer"
    when GcpIdTokenSecret
      header = record.header.presence || "authorization"
      "#{header}: Bearer"
    when AwsAuthSecret  then "Authorization: AWS4-HMAC-SHA256"
    when OauthTokenSecret
      header = record.header.presence || "Authorization"
      prefix = record.value_prefix.presence&.strip
      prefix ? "#{header}: #{prefix} …" : header
    when HmacSecret
      names = Array(record.headers).filter_map { |h| h["name"].presence if h.is_a?(Hash) }
      names.presence&.join(", ")
    when PgDsnSecret then nil
    end
  end

  private

  def source_segment(source, role: nil)
    return nil unless source

    key = SOURCE_REF_KEYS[source.source_type]
    ref =
      if key && source.config.is_a?(Hash)
        source.config[key]
      elsif source.source_type == "control_plane"
        "inline"
      end

    { role: role, type: source.source_type, ref: ref.presence }
  end

  def provider_label(provider)
    return nil unless provider.is_a?(Hash) && provider["type"].present?
    provider["type"]
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
