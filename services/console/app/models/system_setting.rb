class SystemSetting < ApplicationRecord
  # Slack workspace (T...) and Enterprise Grid org (E...) ids.
  SLACK_EXTERNAL_TEAM_ID_FORMAT = /\A[TE][A-Z0-9]+\z/

  attr_readonly :singleton

  before_validation :force_singleton, on: :create
  before_validation :normalize_slack_external_team_ids

  validates :singleton, inclusion: { in: [ true ] }, uniqueness: true
  validates :default_sandbox_repo_cache, inclusion: { in: Principal::SANDBOX_REPO_CACHE_VALUES }
  validates :default_sandbox_observability_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_sessions_read_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_workflows_read_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_workflows_write_enabled, inclusion: { in: [ true, false ] }
  validate :slack_external_team_ids_are_team_ids

  def self.current
    first || create!(singleton: true)
  rescue ActiveRecord::RecordNotUnique
    first
  end

  def principal_defaults
    {
      sandbox_repo_cache: default_sandbox_repo_cache,
      sandbox_observability_enabled: default_sandbox_observability_enabled,
      sandbox_sessions_read_enabled: default_sandbox_sessions_read_enabled,
      sandbox_workflows_read_enabled: default_sandbox_workflows_read_enabled,
      sandbox_workflows_write_enabled: default_sandbox_workflows_write_enabled
    }
  end

  def slack_external_team_ids_text
    slack_external_team_ids.join("\n")
  end

  def slack_external_team_ids_text=(value)
    self.slack_external_team_ids = value.to_s.split(/[\s,]+/)
  end

  private

  def normalize_slack_external_team_ids
    self.slack_external_team_ids = Array(slack_external_team_ids).map { |id| id.to_s.strip.upcase }.compact_blank.uniq
  end

  def slack_external_team_ids_are_team_ids
    invalid = slack_external_team_ids.grep_v(SLACK_EXTERNAL_TEAM_ID_FORMAT)
    errors.add(:slack_external_team_ids, "contains invalid Slack team ids: #{invalid.join(", ")}") if invalid.any?
  end

  def force_singleton
    self.singleton = true
  end
end
