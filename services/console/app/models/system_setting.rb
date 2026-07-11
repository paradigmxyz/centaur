class SystemSetting < ApplicationRecord
  attr_readonly :singleton

  before_validation :force_singleton, on: :create

  validates :singleton, inclusion: { in: [ true ] }, uniqueness: true
  validates :default_sandbox_repo_cache, inclusion: { in: Principal::SANDBOX_REPO_CACHE_VALUES }
  validates :default_sandbox_observability_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_api_server_enabled, inclusion: { in: [ true, false ] }

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

  def force_singleton
    self.singleton = true
  end
end
