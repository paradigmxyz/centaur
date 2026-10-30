# Assembles a principal's effective config, in the shape iron-proxy receives
# from proxy-sync, for operator inspection and sandbox permissions. proxy-sync
# builds the per-proxy config it serves (requester credentials, proxy labels,
# sandbox entitlements, and the config hash) itself.
module PrincipalSyncConfig
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

  def self.sync_secrets_for(principal)
    proxy_secrets_for(served_credentials_for(principal))
  end

  def self.sync_transforms_for(principal)
    proxy_transforms_for(served_credentials_for(principal))
  end

  def self.sync_postgres_for(principal)
    effective_pg_dsn_secrets_for(principal).map { |pg| pg.to_proxy_dsn(principal: principal) }
  end

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
  # and left to proxy order.
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
