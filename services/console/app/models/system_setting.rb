class SystemSetting < ApplicationRecord
  attr_readonly :singleton

  # off     -- serve no `rules`; iron-proxy builds no allowlist transform and
  #            egress is bounded only by NetworkPolicy and the CIDR denylist.
  #            The default, and the behavior of every deployment before this.
  # warn    -- serve the rules, but tell the proxy to log what it *would* have
  #            blocked instead of blocking it. The audit pass before enforcing.
  # enforce -- serve the rules. Anything unmatched gets a 403.
  EGRESS_ALLOWLIST_MODES = %w[off warn enforce].freeze

  before_validation :force_singleton, on: :create

  validates :singleton, inclusion: { in: [ true ] }, uniqueness: true
  validates :default_sandbox_repo_cache, inclusion: { in: Principal::SANDBOX_REPO_CACHE_VALUES }
  validates :default_sandbox_observability_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_api_server_enabled, inclusion: { in: [ true, false ] }
  validates :egress_allowlist_mode, inclusion: { in: EGRESS_ALLOWLIST_MODES }
  validate :base_rules_are_wellformed
  validate :enforcing_needs_a_base_list

  def egress_allowlist_on?
    egress_allowlist_mode != "off"
  end

  def egress_allowlist_warn?
    egress_allowlist_mode == "warn"
  end

  def self.current
    first || create!(singleton: true)
  rescue ActiveRecord::RecordNotUnique
    first
  end

  def principal_defaults
    {
      sandbox_repo_cache: default_sandbox_repo_cache,
      sandbox_observability_enabled: default_sandbox_observability_enabled,
      sandbox_api_server_enabled: default_sandbox_api_server_enabled
    }
  end

  private

  # Turning the allowlist on with nothing in the base list is a sandbox
  # blackout, not a tight boundary. A principal holding no credentials derives
  # no rules, so it would be served `rules: []` -- and iron-proxy's allowlist
  # matches nothing against an empty rule set, so *every* request 403s,
  # including the one to the model provider. Refuse the setting rather than
  # serve a config that bricks every sandbox on the next 5s poll.
  def enforcing_needs_a_base_list
    return unless egress_allowlist_on?
    return if egress_allowlist_base_rules.present?

    errors.add(:egress_allowlist_base_rules,
      "must list at least one host before the allowlist can be turned on " \
      "(an empty allowlist denies everything, including the model provider)")
  end

  def base_rules_are_wellformed
    unless egress_allowlist_base_rules.is_a?(Array)
      errors.add(:egress_allowlist_base_rules, "must be an array")
      return
    end

    egress_allowlist_base_rules.each do |rule|
      unless rule.is_a?(Hash)
        errors.add(:egress_allowlist_base_rules, "each rule must be an object")
        next
      end
      # Same shape iron-proxy's hostmatch.RuleConfig expects, and the same
      # host-xor-cidr law RequestRule enforces for per-secret rules.
      if rule["host"].blank? && rule["cidr"].blank?
        errors.add(:egress_allowlist_base_rules, "each rule needs a host or a cidr")
      elsif rule["host"].present? && rule["cidr"].present?
        errors.add(:egress_allowlist_base_rules, "host and cidr are mutually exclusive")
      end
    end
  end

  def force_singleton
    self.singleton = true
  end
end
