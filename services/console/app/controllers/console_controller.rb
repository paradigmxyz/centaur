# Operator console: a lightweight, server-rendered HTML view over secrets and
# OAuth apps (principals and credentials moved to the SPA). Read-only; gated behind a console session
# (ApplicationController#require_login) and restricted to admins (require_admin),
# like every Control page. Distinct from the JSON API.
class ConsoleController < ApplicationController
  include SecretKinds

  layout "console"

  before_action :require_admin

  SECRETS_PER_PAGE = 50

  # Friendly labels for the source backend (and the gcp_auth credentials_provider
  # type). The secrets table shows only this -- the full reference lives on the
  # secret detail page.
  SOURCE_TYPE_LABELS = {
    "env" => "Env", "aws_sm" => "AWS-SM", "aws_ssm" => "AWS-SSM",
    "1password" => "1Password", "1password_connect" => "1Password-Connect",
    "control_plane" => "Inline", "token_broker" => "Token-Broker",
    "workload_identity" => "Workload-Identity"
  }.freeze

  def secrets
    @search_query = params[:q].to_s.strip
    @selected_secret_type = params[:type].to_s if SECRET_KINDS.key?(params[:type].to_s)
    @total_secrets = SECRET_KINDS.sum { |_kind, cfg| cfg[:model].count }

    kinds = @selected_secret_type ? SECRET_KINDS.slice(@selected_secret_type) : SECRET_KINDS
    @secrets = kinds.flat_map do |kind, cfg|
      rel = cfg[:model].includes(cfg[:includes])
      if @search_query.present?
        pattern = "%#{cfg[:model].sanitize_sql_like(@search_query)}%"
        rel = rel.where("name ILIKE ?", pattern)
      end
      # Static secrets may wrap a broker credential (the "managed" badge); eager
      # load the credential and its app so the list doesn't fan out per row.
      rel = rel.includes(broker_credential: :oauth_app) if cfg[:model] == StaticSecret
      rel.map { |secret| [ kind, secret ] }
    end
    @secrets.sort_by! { |_kind, secret| [ secret.name.to_s.downcase, secret.created_at, secret.id ] }
    @total_count = @secrets.size
    @total_pages = total_pages(@total_count, SECRETS_PER_PAGE)
    @page = bounded_page(params[:page], @total_pages)
    @secrets = @secrets.slice((@page - 1) * SECRETS_PER_PAGE, SECRETS_PER_PAGE) || []
  end

  def secret
    cfg = SECRET_KINDS[params[:kind]]
    return render plain: "secret not found", status: :not_found unless cfg

    @kind = params[:kind]
    @secret = cfg[:model].includes(cfg[:includes]).find_by_oid!(params[:id])
    grantable = @secret.class.name.underscore.to_sym
    @role_grants = Grant.where(grantable => @secret).where.not(role_id: nil).includes(:role).order(:id)
    granted_role_ids = @role_grants.map(&:role_id)
    @assignable_roles = Role.where.not(id: granted_role_ids).order(:id)
  end

  # Registered OAuth apps and the consent flows they drive. Like broker
  # credentials, an app is not grantable -- it is the durable config behind the public
  # /oauth/:provider/start flow.
  def oauth_apps
    @oauth_apps = OauthApp.order(created_at: :asc, id: :asc)
    # One count query for the whole table rather than one per row.
    @minted_counts = BrokerCredential.group(:oauth_app_id).count
  end

  def oauth_app
    @oauth_app = OauthApp.find_by_oid!(params[:id])
    @minted_credentials = @oauth_app.broker_credentials.order(created_at: :asc, id: :asc)
    @provider_details = {}
    @provider_credential = @minted_credentials.first if @oauth_app.shared?
    provider = @oauth_app.provider_strategy
    if provider.respond_to?(:details_for) && @provider_credential && !@provider_credential.dead? &&
        @provider_credential.access_token.present? && (@provider_credential.expires_at.nil? || @provider_credential.expires_at.future?)
      begin
        @provider_details = provider.details_for(@provider_credential,
          http_client: HttpClient.new(open_timeout: 2, read_timeout: 2, write_timeout: 2, max_body_bytes: 128 * 1024))
        @provider_details = {} unless @provider_details.is_a?(Hash)
      rescue StandardError
        # Optional provider details must never prevent credential management.
        @provider_details = {}
      end
    end
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
    when SshKeySecret then [ source_segment(record.key_source) ].compact
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

  def total_pages(total_count, per_page)
    [ (total_count.to_f / per_page).ceil, 1 ].max
  end

  def bounded_page(value, total_pages)
    page = Integer(value.to_s, 10, exception: false) || 1
    [ [ page, 1 ].max, total_pages ].min
  end

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
