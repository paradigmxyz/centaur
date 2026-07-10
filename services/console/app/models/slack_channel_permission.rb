class SlackChannelPermission < ApplicationRecord
  belongs_to :principal

  before_validation :normalize_channel_fields
  before_destroy :prevent_protected_slack_dm_destroy
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
      existing = principal.slack_channel_permissions.index_by(&:channel_id)
      replacement_channel_ids = []

      permission_rows.each do |attrs|
        attrs = attrs.to_h.with_indifferent_access
        channel_id = attrs.fetch(:channel_id, attrs["channel_id"]).to_s.strip.upcase
        replacement_channel_ids << channel_id

        permission = existing[channel_id] || principal.slack_channel_permissions.build
        if permission.protected_slack_dm_permission?
          attrs[:channel_id] = permission.channel_id
          attrs[:channel_name] = permission.channel_name
        end
        permission.assign_attributes(attrs)
        permission.save!
      end

      principal.slack_channel_permissions.reload.each do |permission|
        next if replacement_channel_ids.include?(permission.channel_id)
        next if permission.protected_slack_dm_permission?

        permission.destroy!
      end
    end
  end

  def protected_slack_dm_permission?
    principal_user_id = principal&.slack_user_id
    channel_id.to_s.start_with?("D") &&
      principal_user_id.present? &&
      channel_name.to_s.strip.upcase == principal_user_id
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

  def prevent_protected_slack_dm_destroy
    return if destroyed_by_association.present?
    return unless protected_slack_dm_permission?

    errors.add(:base, "Slack DM permissions created from slack_user_id labels cannot be removed")
    throw :abort
  end

  def bump_principal_sync_config_cache_version
    Principal.bump_sync_config_cache_versions(principal_id)
  end
end
