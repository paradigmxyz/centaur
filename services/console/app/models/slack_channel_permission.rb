class SlackChannelPermission < ApplicationRecord
  belongs_to :principal

  before_validation :normalize_channel_fields
  after_commit :bump_principal_sync_config_cache_version

  validates :channel_id, presence: true,
                         format: { with: Principal::SLACK_CHANNEL_ID_FORMAT, message: "is not a valid Slack channel ID" },
                         uniqueness: { scope: :principal_id }
  validates :upload_enabled, inclusion: { in: [ true, false ] }
  validates :download_enabled, inclusion: { in: [ true, false ] }
  validates :history_enabled, inclusion: { in: [ true, false ] }
  validate :at_least_one_permission

  scope :ordered, -> { order(:channel_id, :id) }

  def self.replace_for_principal!(principal, permission_rows)
    transaction do
      principal.slack_channel_permissions.destroy_all
      permission_rows.each do |attrs|
        principal.slack_channel_permissions.create!(attrs)
      end
    end
  end

  def as_permission_json
    {
      "channel_id" => channel_id,
      "channel_name" => channel_name,
      "upload_enabled" => upload_enabled,
      "download_enabled" => download_enabled,
      "history_enabled" => history_enabled
    }
  end

  private

  def normalize_channel_fields
    self.channel_id = channel_id.to_s.strip.upcase
    self.channel_name = channel_name.to_s.strip.presence
  end

  def at_least_one_permission
    return if upload_enabled || download_enabled || history_enabled
    errors.add(:base, "Select at least one Slack permission")
  end

  def bump_principal_sync_config_cache_version
    Principal.bump_sync_config_cache_versions(principal_id)
  end
end
