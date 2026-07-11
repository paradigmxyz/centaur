class SystemSetting < ApplicationRecord
  DEFAULT_SANDBOX_REPO_CACHE = "all".freeze
  DEFAULT_SANDBOX_OBSERVABILITY_ENABLED = true
  DEFAULT_SANDBOX_API_SERVER_ENABLED = true

  attr_readonly :singleton

  before_validation :force_singleton, on: :create
  before_validation :normalize_default_sandbox_repo_cache

  validates :singleton, inclusion: { in: [ true ] }, uniqueness: true
  validates :default_sandbox_repo_cache, inclusion: { in: Principal::SANDBOX_REPO_CACHE_VALUES }
  validates :default_sandbox_observability_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_api_server_enabled, inclusion: { in: [ true, false ] }

  def self.current
    first || create!(singleton: true)
  rescue ActiveRecord::RecordNotUnique
    first
  end

  def self.normalize_repo_cache(value)
    normalized = value.to_s.strip.downcase
    normalized = Principal::SANDBOX_REPO_CACHE_ALIASES.fetch(normalized, normalized)
    return normalized if Principal::SANDBOX_REPO_CACHE_VALUES.include?(normalized)

    DEFAULT_SANDBOX_REPO_CACHE
  end

  def principal_defaults
    {
      sandbox_repo_cache: default_sandbox_repo_cache,
      sandbox_observability_enabled: default_sandbox_observability_enabled,
      sandbox_api_server_enabled: default_sandbox_api_server_enabled
    }
  end

  private

  def force_singleton
    self.singleton = true
  end

  def normalize_default_sandbox_repo_cache
    self.default_sandbox_repo_cache = self.class.normalize_repo_cache(default_sandbox_repo_cache)
  end
end
