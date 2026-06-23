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
  has_many :sync_config_snapshots, class_name: "PrincipalSyncConfigSnapshot", dependent: :destroy
  belongs_to :created_by, class_name: "User"

  before_commit :bump_own_sync_config_cache_version, on: :update, if: :sync_config_fields_changed?

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

  # gcp_id_token credentials this principal resolves to, via its effective grants.
  def granted_gcp_id_token_secrets
    granted_secrets_by_priority(GcpIdTokenSecret, :gcp_id_token_secret_id, includes: %i[keyfile_source rules])
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
  # `secrets` transform `secretEntry` shape. The served set (see
  # #served_credentials) has already dropped secrets without a deliverable source
  # and the losers of any cross-type conflict.
  def sync_secrets
    proxy_secrets_for(served_credentials)
  end

  # The `transforms` array delivered to iron-proxy: one gcp_auth/gcp_id_token/
  # aws_auth/hmac_sign transform per granted secret, plus a single oauth_token
  # transform bundling every granted OauthTokenSecret as one `tokens` entry.
  # Credentials that lost a cross-type conflict are omitted (see
  # #served_credentials).
  def sync_transforms
    proxy_transforms_for(served_credentials)
  end

  # The top-level `postgres` array delivered to iron-proxy: one effective DSN
  # entry per database. Entries without a DSN source are skipped because the
  # proxy can't dial an upstream without one. When several granted PG DSNs route
  # the same database, existing grant priority ordering decides the winner:
  # higher-priority grants appear later and overwrite lower-priority routes.
  def sync_postgres
    winners = {}
    granted_pg_dsn_secrets.each do |pg|
      next unless pg.dsn_source
      winners[pg.database] = pg
    end
    winners.values.map { |pg| pg.to_proxy_dsn(principal: self) }
  end

  # The config this principal resolves to, in the same shape iron-proxy receives
  # on /sync, but for operator inspection rather than delivery: when
  # `redact_secrets` is set (the default), inline control_plane source values are
  # replaced with REDACTED. Every other source type carries a reference (an env
  # var name, a secret_id, ...) that is configuration, not a live credential, so
  # it passes through untouched.
  def effective_config(redact_secrets: true)
    served = served_credentials
    config = {
      "secrets" => proxy_secrets_for(served),
      "transforms" => proxy_transforms_for(served),
      "postgres" => sync_postgres
    }
    redact_secrets ? self.class.redact_live_secrets(config) : config
  end

  def self.bump_sync_config_cache_versions(ids)
    ids = Array(ids).compact.uniq
    return if ids.empty?

    where(id: ids).update_all("sync_config_cache_version = sync_config_cache_version + 1")
  end

  def self.effective_grantee_ids_for_grantable(grantable)
    association = grantable.model_name.singular.to_sym
    grants = Grant.where(association => grantable)
    direct_ids = grants.where.not(principal_id: nil).pluck(:principal_id)
    role_ids = grants.where.not(role_id: nil).pluck(:role_id)
    role_principal_ids = role_ids.empty? ? [] : PrincipalRole.where(role_id: role_ids).pluck(:principal_id)
    direct_ids + role_principal_ids
  end

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

  private

  # The credentials actually delivered to the proxy, grouped by type, after
  # cross-type conflict resolution. Static secrets without a deliverable source
  # are dropped first (the proxy can't resolve a value for them) so a
  # non-deliverable winner never suppresses a credential that would otherwise
  # serve. The result is recomputed on each call so callers see live grant state.
  def served_credentials
    static = granted_static_secrets.select { |ss| ss.source&.deliverable? }
    gcp_auth = granted_gcp_auth_secrets.to_a
    gcp_id_token = granted_gcp_id_token_secrets.to_a
    aws_auth = granted_aws_auth_secrets.to_a
    hmac = granted_hmac_secrets.to_a
    oauth = granted_oauth_token_secrets.to_a

    suppressed = suppressed_conflict_credentials(static + gcp_auth + gcp_id_token + aws_auth + hmac + oauth)

    {
      static: static - suppressed,
      gcp_auth: gcp_auth - suppressed,
      gcp_id_token: gcp_id_token - suppressed,
      aws_auth: aws_auth - suppressed,
      hmac: hmac - suppressed,
      oauth: oauth - suppressed
    }
  end

  def proxy_secrets_for(served)
    served[:static].map(&:to_proxy_secret)
  end

  def proxy_transforms_for(served)
    transforms = served[:gcp_auth].map(&:to_proxy_transform)
    transforms += served[:gcp_id_token].map(&:to_proxy_transform)
    transforms += served[:aws_auth].map(&:to_proxy_transform)
    transforms += served[:hmac].map(&:to_proxy_transform)

    oauth_entries = served[:oauth].map(&:to_proxy_entry)
    transforms << { "name" => "oauth_token", "config" => { "tokens" => oauth_entries } } if oauth_entries.any?

    transforms
  end

  # Cross-type conflict resolution. The wire protocol applies the `secrets` array
  # (static secrets) before the `transforms` array (gcp_auth, aws_auth, hmac_sign,
  # oauth_token), so the proxy's last-transform-wins cannot let a direct static
  # secret beat a role-granted transform. We resolve it here instead: each
  # credential claims the host/cidr scopes and header/query params it writes;
  # processing claimants strongest-first, any credential overlapping a claim a
  # stronger one already took is withheld. Strength is the effective grant
  # priority (direct outranks role). Same-priority conflicts are still emitted
  # and left to proxy order, with id and class name only keeping config_hash
  # stable.
  #
  # Host matching follows the proxy's wildcard behavior closely enough for
  # conflict detection: exact hosts collide with matching wildcard hosts (for
  # example `gmail.googleapis.com` and `*.googleapis.com`). Method/path narrowing
  # on a rule is ignored. This may suppress credentials with disjoint paths, but
  # it prevents a lower-priority transform from overwriting a higher-priority
  # header at runtime.
  def suppressed_conflict_credentials(credentials)
    candidates = credentials.filter_map do |cred|
      claims = conflict_claims_for(cred)
      [ cred, claims ] unless claims.empty?
    end

    candidates.sort_by! do |cred, _keys|
      [ -cred.effective_priority.to_i, -cred.id, cred.class.name ]
    end

    claimed_by_target = Hash.new { |hash, target| hash[target] = [] }
    suppressed = []
    candidates.each do |cred, claims|
      priority = cred.effective_priority.to_i
      if claims.any? { |claim| stronger_claim_exists?(claim, priority, claimed_by_target) }
        suppressed << cred
      else
        claims.each do |claim|
          _scope, target = claim
          claimed_by_target[target] << { claim: claim, priority: priority }
        end
      end
    end
    suppressed
  end

  # The [scope, target] claims a credential writes: the cross product of the
  # hosts/cidrs its rules match and the headers/params it injects. Empty when the
  # credential scopes nothing (no rules) or writes no header/param target, in
  # which case it never participates in a conflict.
  def conflict_claims_for(cred)
    scopes = cred.rules.filter_map { |rule| conflict_scope(rule) }
    targets = cred.proxy_conflict_targets
    return [] if scopes.empty? || targets.empty?
    scopes.product(targets)
  end

  def stronger_claim_exists?(claim, priority, claimed_by_target)
    _scope, target = claim
    claimed_by_target[target].any? do |prior|
      prior[:priority] > priority && conflict_claims_overlap?(claim, prior[:claim])
    end
  end

  def conflict_scope(rule)
    if rule.host.present?
      { type: :host, value: rule.host.strip.downcase.delete_suffix(".") }
    elsif rule.cidr.present?
      { type: :cidr, value: rule.cidr }
    end
  end

  def conflict_claims_overlap?(claim, other_claim)
    scope, target = claim
    other_scope, other_target = other_claim
    target == other_target && conflict_scopes_overlap?(scope, other_scope)
  end

  def conflict_scopes_overlap?(scope, other_scope)
    return false unless scope[:type] == other_scope[:type]
    return scope[:value] == other_scope[:value] if scope[:type] == :cidr

    host_patterns_overlap?(scope[:value], other_scope[:value])
  end

  def host_patterns_overlap?(pattern, other_pattern)
    return true if pattern == other_pattern
    return true if pattern == "*" || other_pattern == "*"

    labels = pattern.split(".")
    other_labels = other_pattern.split(".")
    return false unless labels.length == other_labels.length

    labels.zip(other_labels).all? do |label, other_label|
      label == "*" || other_label == "*" || label == other_label
    end
  end

  # The single place secret order is decided for every sync array. iron-proxy
  # applies matching transforms in array order and the LAST one wins, so we emit
  # in ASCENDING priority: the highest-priority grant lands last and becomes
  # authoritative. A secret reachable by several grants (e.g. both directly and
  # via a role) collapses to one row taking the strongest priority among them
  # (MAX), and the id tiebreak keeps the order deterministic for config_hash.
  # The selected effective_priority also drives cross-type conflict resolution
  # (see #suppressed_conflict_credentials).
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
      .select("#{model.table_name}.*", "granted_priorities.effective_priority")
      .includes(*includes)
      .order(Arel.sql("granted_priorities.effective_priority ASC, #{model.table_name}.id ASC"))
  end

  def sync_config_fields_changed?
    previous_changes.key?("name") || previous_changes.key?("labels")
  end

  def bump_own_sync_config_cache_version
    self.class.bump_sync_config_cache_versions(id)
  end
end
