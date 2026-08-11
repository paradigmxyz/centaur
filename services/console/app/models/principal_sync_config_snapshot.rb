class PrincipalSyncConfigSnapshot < ApplicationRecord
  TTL = 10.minutes
  RETENTION = 1.hour

  belongs_to :principal

  encrypts :payload
  serialize :payload, coder: JSON

  validates :principal_cache_version, presence: true
  validates :principal_id, uniqueness: { scope: :principal_cache_version }

  def config
    payload.fetch("config", payload)
  end

  def postgres_setting_templates
    payload.fetch("postgres_setting_templates", {})
  end

  def self.payload_for(principal)
    served = served_credentials_for(principal)
    postgres, templates = sync_postgres_entries_with_templates_for(principal)
    {
      "config" => {
        "secrets" => proxy_secrets_for(served) + generated_proxy_secrets_for(principal),
        "transforms" => proxy_transforms_for(served),
        "postgres" => postgres
      },
      "postgres_setting_templates" => templates
    }
  end

  def self.config_for(principal)
    served = served_credentials_for(principal)
    {
      "secrets" => proxy_secrets_for(served) + generated_proxy_secrets_for(principal),
      "transforms" => proxy_transforms_for(served),
      "postgres" => sync_postgres_for(principal)
    }
  end

  def self.redacted_config_for(principal)
    Principal.redact_live_secrets(config_for(principal))
  end

  def self.sync_config_for_proxy(proxy, sandbox_entitlements_hosts: Proxy.sandbox_entitlements_hosts)
    config = config_for_proxy(proxy, sandbox_entitlements_hosts: sandbox_entitlements_hosts)
    { config_hash: config_hash_for_proxy(proxy, config), config: config }
  end

  def self.config_for_proxy(proxy, sandbox_entitlements_hosts: Proxy.sandbox_entitlements_hosts)
    config = rendered_principal_config_for_proxy(proxy)
    with_sandbox_entitlements_secret_for_proxy(proxy, config, hosts: sandbox_entitlements_hosts)
  end

  def self.sync_secrets_for(principal)
    proxy_secrets_for(served_credentials_for(principal))
  end

  def self.sync_transforms_for(principal)
    proxy_transforms_for(served_credentials_for(principal))
  end

  def self.sync_postgres_for(principal, proxy: nil)
    effective_pg_dsn_secrets_for(principal).map { |pg| pg.to_proxy_dsn(principal: principal, proxy: proxy) }
  end

  def self.config_hash_for_proxy(proxy, config)
    payload = config.merge(
      "principal" => proxy.principal&.oid,
      "principal_assigned_at" => proxy.principal_assigned_at&.utc&.iso8601,
      "proxy_labels" => proxy.labels || {}
    )
    "sha256:#{Digest::SHA256.hexdigest(canonical_json(payload))}"
  end

  # Returns the freshest usable snapshot, stale-while-revalidate style. Config
  # invalidations only bump the principal cache version; polling requests serve
  # stale snapshots immediately and enqueue the background rebuild on demand
  # instead of using request threads and DB connections to rebuild the effective
  # config.
  #
  # Serving a stale snapshot is safe: iron-proxy treats the config hash as an
  # ETag and re-applies on its next 5s poll once the rebuild lands. Only a
  # cold start (no snapshot at any version) blocks until the build finishes,
  # because there is nothing stale to serve.
  def self.fetch_for(principal)
    version = principal.sync_config_cache_version
    snapshot = find_by(principal: principal, principal_cache_version: version)
    return snapshot if snapshot&.fresh_for?(principal)

    stale = snapshot || latest_for(principal)
    if stale
      Principal.enqueue_sync_config_snapshot_warm(principal.id)
      return stale
    end

    build_for(principal)
  end

  def self.prune_expired!
    where("updated_at < ?", RETENTION.ago).delete_all
  end

  def fresh?
    updated_at >= TTL.ago
  end

  def fresh_for?(principal)
    fresh? && !api_server_jwt_window_stale?(principal)
  end

  # Most recent snapshot at any cache version; the stale fallback while
  # another session rebuilds. Old versions survive until prune_expired!
  # (RETENTION), which comfortably covers a rebuild.
  def self.latest_for(principal)
    where(principal: principal).order(updated_at: :desc).first
  end

  def self.build_for(principal)
    principal.with_lock { build_within_lock(principal) }
  rescue ActiveRecord::RecordNotUnique
    retry
  end

  # Assumes the caller holds the principal's row lock and passes the freshly
  # locked (reloaded) record, so sync_config_cache_version is current.
  def self.build_within_lock(principal)
    version = principal.sync_config_cache_version
    snapshot = find_or_initialize_by(principal: principal, principal_cache_version: version)
    return snapshot if snapshot.persisted? && snapshot.fresh_for?(principal)

    snapshot.payload = payload_for(principal)
    if snapshot.changed?
      snapshot.save!
    else
      # A rebuild that yields an identical payload must still restart the TTL,
      # or the snapshot stays permanently stale and every poll re-runs the
      # expensive config rebuild.
      snapshot.touch
    end
    snapshot
  end

  def api_server_jwt_window_stale?(principal)
    return false unless principal.sandbox_api_server_enabled?

    return false if principal.slack_jwt_channel_ids.empty?
    return false if ENV["CENTAUR_JWT_SIGNING_SECRET"].to_s.blank?

    updated_at.to_i < ApiServer::Jwt.window_start_for(principal, Time.current.to_i)
  end

  def self.rendered_principal_config_for_proxy(proxy)
    principal = proxy.principal
    return Principal::EMPTY_CONFIG.deep_dup unless principal

    snapshot = fetch_for(principal)
    copy = snapshot.config.deep_dup
    templates = snapshot.postgres_setting_templates
    copy["postgres"] = proxy_specific_postgres(proxy, copy["postgres"], templates) if templates.any?
    copy
  end
  private_class_method :rendered_principal_config_for_proxy

  def self.proxy_specific_postgres(proxy, postgres, templates)
    Array(postgres).map do |entry|
      next entry unless entry.is_a?(Hash)

      template = templates[entry["id"].to_s]
      next entry unless template

      rendered_settings = proxy_specific_postgres_settings(proxy, entry["settings"], template)
      entry.merge("settings" => rendered_settings)
    end
  end
  private_class_method :proxy_specific_postgres

  def self.proxy_specific_postgres_settings(proxy, rendered_settings, template_settings)
    rendered_by_name = Array(rendered_settings).each_with_object({}) do |setting, values|
      next unless setting.is_a?(Hash)

      name = setting["name"].presence || setting[:name].presence
      values[name] = setting["value"] || setting[:value] if name.present?
    end

    Array(template_settings).filter_map do |setting|
      next unless setting.is_a?(Hash)

      name = setting["name"].presence || setting[:name].presence
      next if name.blank?

      value = proxy_label_setting_value(proxy, setting)
      value = rendered_by_name.fetch(name, "") if value.nil?
      { "name" => name, "value" => value }
    end
  end
  private_class_method :proxy_specific_postgres_settings

  def self.proxy_label_setting_value(proxy, setting)
    ref = setting["value_from"] || setting[:value_from]
    return nil unless ref.is_a?(Hash)

    proxy_label = ref["proxy_label"] || ref[:proxy_label]
    return nil if proxy_label.blank?

    proxy.labels&.fetch(proxy_label.to_s, "").to_s
  end
  private_class_method :proxy_label_setting_value

  def self.with_sandbox_entitlements_secret_for_proxy(proxy, config, hosts:)
    secret = sandbox_entitlements_secret_for_proxy(proxy, hosts: hosts)
    return config unless secret

    config.deep_dup.tap do |copy|
      copy["secrets"] = Array(copy["secrets"]) + [ secret ]
    end
  end
  private_class_method :with_sandbox_entitlements_secret_for_proxy

  def self.sandbox_entitlements_secret_for_proxy(proxy, hosts:)
    rules = Principal.normalize_hosts(hosts)
      .map do |host|
        {
          "host" => host,
          "methods" => %w[GET POST PUT PATCH DELETE],
          "paths" => [ Proxy::SANDBOX_ENTITLEMENTS_PATH_PATTERN ]
        }
      end
    return nil if rules.empty?

    token = SandboxEntitlements::Jwt.encode_for_proxy(proxy)
    return nil if token.blank?

    {
      "source" => { "type" => "control_plane", "value" => token },
      "inject" => { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      "rules" => rules
    }
  end
  private_class_method :sandbox_entitlements_secret_for_proxy

  # Deep key-sorted JSON so the hash is stable regardless of Hash insertion or
  # jsonb column ordering.
  def self.canonical_json(value)
    JSON.generate(canonicalize(value))
  end
  private_class_method :canonical_json

  def self.canonicalize(value)
    case value
    when Hash
      value.sort_by { |k, _| k.to_s }.to_h.transform_values { |v| canonicalize(v) }
    when Array
      value.map { |v| canonicalize(v) }
    else
      value
    end
  end
  private_class_method :canonicalize

  def self.sync_postgres_entries_with_templates_for(principal)
    templates = {}
    entries = effective_pg_dsn_secrets_for(principal).map do |pg|
      templates[pg.oid] = pg.settings if pg.proxy_label_settings?
      pg.to_proxy_dsn(principal: principal)
    end
    [ entries, templates ]
  end
  private_class_method :sync_postgres_entries_with_templates_for

  def self.effective_pg_dsn_secrets_for(principal)
    principal.granted_pg_dsn_secrets.each_with_object({}) do |pg, winners|
      winners[pg.database] = pg if pg.dsn_source
    end.values
  end
  private_class_method :effective_pg_dsn_secrets_for

  # The credentials actually delivered to the proxy, grouped by type, after
  # cross-type conflict resolution. Static secrets without a deliverable source
  # are dropped first (the proxy can't resolve a value for them) so a
  # non-deliverable winner never suppresses a credential that would otherwise
  # serve.
  def self.served_credentials_for(principal)
    static = principal.granted_static_secrets.select { |ss| ss.source&.deliverable? }
    gcp_auth = principal.granted_gcp_auth_secrets.to_a
    gcp_id_token = principal.granted_gcp_id_token_secrets.to_a
    aws_auth = principal.granted_aws_auth_secrets.to_a
    hmac = principal.granted_hmac_secrets.to_a
    oauth = principal.granted_oauth_token_secrets.to_a

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
  private_class_method :served_credentials_for

  def self.proxy_secrets_for(served)
    served[:static].map(&:to_proxy_secret)
  end
  private_class_method :proxy_secrets_for

  def self.generated_proxy_secrets_for(principal)
    secret = api_server_jwt_secret_for(principal)
    secret ? [ secret ] : []
  end
  private_class_method :generated_proxy_secrets_for

  def self.api_server_jwt_secret_for(principal)
    return nil unless principal.sandbox_api_server_enabled?

    token = ApiServer::Jwt.encode_for_principal(principal)
    return nil if token.blank?

    rules = api_server_hosts_for.map { |host| { "host" => host } }
    return nil if rules.empty?

    {
      "source" => { "type" => "control_plane", "value" => token },
      "inject" => { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      "rules" => rules
    }
  end
  private_class_method :api_server_jwt_secret_for

  def self.api_server_hosts_for
    configured = ENV["CENTAUR_API_SERVER_PROXY_HOSTS"].to_s.split(",")
    from_url = Principal.host_from_url(ENV["CENTAUR_API_URL"])
    Principal.normalize_hosts(configured + [ from_url ])
  end
  private_class_method :api_server_hosts_for

  def self.proxy_transforms_for(served)
    transforms = served[:gcp_auth].map(&:to_proxy_transform)
    transforms += served[:gcp_id_token].map(&:to_proxy_transform)
    transforms += served[:aws_auth].map(&:to_proxy_transform)
    transforms += served[:hmac].map(&:to_proxy_transform)

    oauth_entries = served[:oauth].map(&:to_proxy_entry)
    transforms << { "name" => "oauth_token", "config" => { "tokens" => oauth_entries } } if oauth_entries.any?

    transforms
  end
  private_class_method :proxy_transforms_for

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
  def self.suppressed_conflict_credentials(credentials)
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
  private_class_method :suppressed_conflict_credentials

  # The [scope, target] claims a credential writes: the cross product of the
  # hosts/cidrs its rules match and the headers/params it injects. Empty when the
  # credential scopes nothing (no rules) or writes no header/param target, in
  # which case it never participates in a conflict.
  def self.conflict_claims_for(cred)
    scopes = cred.rules.filter_map { |rule| conflict_scope(rule) }
    targets = cred.proxy_conflict_targets
    return [] if scopes.empty? || targets.empty?
    scopes.product(targets)
  end
  private_class_method :conflict_claims_for

  def self.stronger_claim_exists?(claim, priority, claimed_by_target)
    _scope, target = claim
    claimed_by_target[target].any? do |prior|
      prior[:priority] > priority && conflict_claims_overlap?(claim, prior[:claim])
    end
  end
  private_class_method :stronger_claim_exists?

  def self.conflict_scope(rule)
    if rule.host.present?
      { type: :host, value: rule.host.strip.downcase.delete_suffix(".") }
    elsif rule.cidr.present?
      { type: :cidr, value: rule.cidr }
    end
  end
  private_class_method :conflict_scope

  def self.conflict_claims_overlap?(claim, other_claim)
    scope, target = claim
    other_scope, other_target = other_claim
    target == other_target && conflict_scopes_overlap?(scope, other_scope)
  end
  private_class_method :conflict_claims_overlap?

  def self.conflict_scopes_overlap?(scope, other_scope)
    return false unless scope[:type] == other_scope[:type]
    return scope[:value] == other_scope[:value] if scope[:type] == :cidr

    host_patterns_overlap?(scope[:value], other_scope[:value])
  end
  private_class_method :conflict_scopes_overlap?

  def self.host_patterns_overlap?(pattern, other_pattern)
    return true if pattern == other_pattern
    return true if pattern == "*" || other_pattern == "*"

    labels = pattern.split(".")
    other_labels = other_pattern.split(".")
    return false unless labels.length == other_labels.length

    labels.zip(other_labels).all? do |label, other_label|
      label == "*" || other_label == "*" || label == other_label
    end
  end
  private_class_method :host_patterns_overlap?
end
