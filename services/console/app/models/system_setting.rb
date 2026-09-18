class SystemSetting < ApplicationRecord
  belongs_to :published_organization_instruction_version,
             class_name: "OrganizationInstructionVersion",
             optional: true

  attr_readonly :singleton

  before_validation :force_singleton, on: :create

  validates :singleton, inclusion: { in: [ true ] }, uniqueness: true
  validates :default_sandbox_repo_cache, inclusion: { in: Principal::SANDBOX_REPO_CACHE_VALUES }
  validates :default_sandbox_observability_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_sessions_read_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_workflows_read_enabled, inclusion: { in: [ true, false ] }
  validates :default_sandbox_workflows_write_enabled, inclusion: { in: [ true, false ] }
  validates :organization_instructions_draft, length: { maximum: 32_000 }

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

  def publish_organization_instructions!(published_by:)
    with_lock do
      publish_organization_instructions_without_lock!(published_by: published_by)
    end
  end

  def restore_organization_instructions!(version:, published_by:)
    with_lock do
      update!(organization_instructions_draft: version.content)
      publish_organization_instructions_without_lock!(published_by: published_by)
    end
  end

  private

  def force_singleton
    self.singleton = true
  end

  def publish_organization_instructions_without_lock!(published_by:)
    version = OrganizationInstructionVersion.create!(
      content: organization_instructions_draft.to_s,
      published_by: published_by
    )
    update!(published_organization_instruction_version: version)
    version
  end
end
