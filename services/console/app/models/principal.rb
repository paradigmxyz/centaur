class Principal < ApplicationRecord
  oid_prefix "prn"

  include ForeignIdCollisionGuard

  attr_readonly :namespace, :foreign_id

  has_many :grants, dependent: :destroy
  # Proxies outlive their principal: deleting a principal unassigns its proxies
  # rather than destroying them, leaving them ready for reassignment.
  has_many :proxies, dependent: :nullify
  has_many :principal_roles, dependent: :destroy
  has_many :roles, through: :principal_roles
  belongs_to :created_by, class_name: "User"

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true

  # Stand-in for an inline secret value in redacted config: effective_config
  # reports that a control_plane source carries a value without revealing it.
  REDACTED = "[redacted]".freeze

  # The config of a principal with no effective grants; also what an unassigned
  # proxy resolves to.
  EMPTY_CONFIG = { "secrets" => [], "transforms" => [], "postgres" => [] }.freeze

  # Every grant this principal resolves to: its own direct grants plus the
  # grants of every role it is assigned. Secrets reachable through more than one
  # path collapse naturally because callers select distinct secret rows.
  def effective_grants
    Grant.where(principal_id: id).or(Grant.where(role_id: role_ids))
  end

  # Static secrets this principal resolves to, via its effective grants.
  def granted_static_secrets
    granted_secrets_by_priority(StaticSecret, :static_secret_id, includes: %i[source rules])
  end

  # gcp_auth credentials this principal resolves to, via its effective grants.
  def granted_gcp_auth_secrets
    granted_secrets_by_priority(GcpAuthSecret, :gcp_auth_secret_id, includes: %i[keyfile_source rules])
  end

  # aws_auth credentials this principal resolves to, via its effective grants.
  def granted_aws_auth_secrets
    granted_secrets_by_priority(AwsAuthSecret, :aws_auth_secret_id, includes: %i[sources rules])
  end

  # oauth_token credentials this principal resolves to, via its effective grants.
  def granted_oauth_token_secrets
    granted_secrets_by_priority(OauthTokenSecret, :oauth_token_secret_id, includes: %i[sources rules])
  end

  # hmac_sign credentials this principal resolves to, via its effective grants.
  def granted_hmac_secrets
    granted_secrets_by_priority(HmacSecret, :hmac_secret_id, includes: %i[sources rules])
  end

  # Postgres upstreams this principal resolves to, via its effective grants.
  def granted_pg_dsn_secrets
    granted_secrets_by_priority(PgDsnSecret, :pg_dsn_secret_id, includes: %i[dsn_source])
  end

  # The `secrets` array delivered to iron-proxy. Each entry maps to the proxy's
  # `secrets` transform `secretEntry` shape. Secrets without a source are skipped
  # because the proxy requires a source to resolve a value; a brokered source whose
  # credential has no current access token (bootstrapping or dead) is skipped too,
  # so the proxy never receives an empty inline value.
  def sync_secrets
    granted_static_secrets.filter_map do |ss|
      next unless ss.source&.deliverable?
      ss.to_proxy_secret
    end
  end

  # The `transforms` array delivered to iron-proxy: one gcp_auth transform per
  # granted GcpAuthSecret, one hmac_sign transform per granted HmacSecret, plus a
  # single oauth_token transform bundling every granted OauthTokenSecret as one
  # `tokens` entry.
  def sync_transforms
    transforms = granted_gcp_auth_secrets.map(&:to_proxy_transform)
    transforms += granted_aws_auth_secrets.map(&:to_proxy_transform)
    transforms += granted_hmac_secrets.map(&:to_proxy_transform)

    oauth_entries = granted_oauth_token_secrets.map(&:to_proxy_entry)
    transforms << { "name" => "oauth_token", "config" => { "tokens" => oauth_entries } } if oauth_entries.any?

    transforms
  end

  # The top-level `postgres` array delivered to iron-proxy: one DSN entry per
  # granted PgDsnSecret, keyed by foreign_id. Entries without a DSN source are
  # skipped because the proxy can't dial an upstream without one.
  def sync_postgres
    granted_pg_dsn_secrets.filter_map do |pg|
      next unless pg.dsn_source
      pg.to_proxy_dsn(principal: self)
    end
  end

  # The config this principal resolves to, in the same shape iron-proxy receives
  # on /sync, but for operator inspection rather than delivery: when
  # `redact_secrets` is set (the default), inline control_plane source values are
  # replaced with REDACTED. Every other source type carries a reference (an env
  # var name, a secret_id, ...) that is configuration, not a live credential, so
  # it passes through untouched.
  def effective_config(redact_secrets: true)
    config = {
      "secrets" => sync_secrets,
      "transforms" => sync_transforms,
      "postgres" => sync_postgres
    }
    redact_secrets ? self.class.redact_live_secrets(config) : config
  end

  private

  # The single place secret order is decided for every sync array. iron-proxy
  # applies matching transforms in array order and the LAST one wins, so we emit
  # in ASCENDING priority: the highest-priority grant lands last and becomes
  # authoritative. A secret reachable by several grants (e.g. both directly and
  # via a role) collapses to one row taking the strongest priority among them
  # (MAX), and the id tiebreak keeps the order deterministic for config_hash.
  #
  # Do NOT add an `.order(:id)`-style sort to the per-type callers above or emit
  # grants in any other order downstream: that would silently let the wrong
  # credential win. `foreign_key` and the model table name are internal symbols,
  # never user input.
  def granted_secrets_by_priority(model, foreign_key, includes:)
    priorities = effective_grants
      .where.not(foreign_key => nil)
      .group(foreign_key)
      .select("#{foreign_key} AS secret_id, MAX(priority) AS effective_priority")

    model
      .joins("INNER JOIN (#{priorities.to_sql}) granted_priorities " \
             "ON granted_priorities.secret_id = #{model.table_name}.id")
      .includes(*includes)
      .order(Arel.sql("granted_priorities.effective_priority ASC, #{model.table_name}.id ASC"))
  end

  public

  # Deep-walk a config payload and blank out the inline value of every
  # control_plane source, leaving the rest of the structure intact.
  def self.redact_live_secrets(value)
    case value
    when Hash
      redacted = value.transform_values { |v| redact_live_secrets(v) }
      redacted["value"] = REDACTED if redacted["type"] == "control_plane" && redacted.key?("value")
      redacted
    when Array
      value.map { |v| redact_live_secrets(v) }
    else
      value
    end
  end
end
