class SlackChannelPermission < ApplicationRecord
  CACHE_BUMP_SUPPRESSED_KEY = :slack_channel_permission_cache_bump_suppressed

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

  def self.replace_for_principal!(principal, permission_rows, channel_names_by_id: {})
    rows = normalize_rows(permission_rows, channel_names_by_id: channel_names_by_id)
    suppress_principal_sync_config_cache_bump do
      transaction do
        principal.slack_channel_permissions.destroy_all
        rows.each do |attrs|
          principal.slack_channel_permissions.create!(attrs)
        end
      end
    end
    Principal.bump_sync_config_cache_versions(principal.id)
  end

  def self.normalize_rows(permission_rows, channel_names_by_id: {})
    boolean = ActiveModel::Type::Boolean.new
    seen = {}
    Array(permission_rows).each do |row|
      next unless row.is_a?(Hash)
      next if boolean.cast(param(row, :remove))

      channel_id = param(row, :channel_id).to_s.strip.upcase
      next if channel_id.blank?

      upload_enabled = boolean.cast(param(row, :upload_enabled))
      download_enabled = boolean.cast(param(row, :download_enabled))
      history_enabled = boolean.cast(param(row, :history_enabled))
      next unless upload_enabled || download_enabled || history_enabled

      channel_name = channel_names_by_id[channel_id].presence || param(row, :channel_name).to_s.strip.presence
      attrs = seen[channel_id]
      unless attrs
        attrs = seen[channel_id] = {
          channel_id: channel_id,
          channel_name: channel_name,
          upload_enabled: false,
          download_enabled: false,
          history_enabled: false
        }
      end
      attrs[:channel_name] ||= channel_name
      attrs[:upload_enabled] = true if upload_enabled
      attrs[:download_enabled] = true if download_enabled
      attrs[:history_enabled] = true if history_enabled
    end
    seen.values
  end

  def self.suppress_principal_sync_config_cache_bump
    previous = Thread.current[CACHE_BUMP_SUPPRESSED_KEY]
    Thread.current[CACHE_BUMP_SUPPRESSED_KEY] = true
    yield
  ensure
    Thread.current[CACHE_BUMP_SUPPRESSED_KEY] = previous
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

  def self.param(row, key)
    string_key = key.to_s
    return row[string_key] if row.key?(string_key)
    return row[key] if row.key?(key)
    nil
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
    return if Thread.current[self.class::CACHE_BUMP_SUPPRESSED_KEY]

    Principal.bump_sync_config_cache_versions(principal_id)
  end
end
