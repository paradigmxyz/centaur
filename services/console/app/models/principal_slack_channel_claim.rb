class PrincipalSlackChannelClaim < ApplicationRecord
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

  def self.replace_for_principal!(principal, raw_claims, channel_names_by_id: {})
    rows = normalize_rows(raw_claims, channel_names_by_id: channel_names_by_id)
    transaction do
      principal.principal_slack_channel_claims.destroy_all
      rows.each do |attrs|
        principal.principal_slack_channel_claims.create!(attrs)
      end
    end
  end

  def self.normalize_rows(raw_claims, channel_names_by_id: {})
    rows = case raw_claims
           when ActionController::Parameters
             normalize_rows(raw_claims.to_unsafe_h, channel_names_by_id: channel_names_by_id)
           when Hash
             if raw_claims.keys.all? { |key| key.to_s.match?(/\A\d+\z/) }
               raw_claims.sort_by { |key, _row| key.to_i }.map(&:last)
             else
               [ raw_claims ]
             end
           else
             Array(raw_claims)
           end

    boolean = ActiveModel::Type::Boolean.new
    seen = {}
    rows.each do |row|
      row = row.to_unsafe_h if row.respond_to?(:to_unsafe_h)
      next unless row.is_a?(Hash)
      next if boolean.cast(param(row, :remove))

      channel_id = param(row, :channel_id).to_s.strip.upcase
      next if channel_id.blank?

      upload_enabled = boolean.cast(param(row, :upload_enabled))
      download_enabled = boolean.cast(param(row, :download_enabled))
      history_enabled = boolean.cast(param(row, :history_enabled))
      next unless upload_enabled || download_enabled || history_enabled

      attrs = seen[channel_id] ||= {
        channel_id: channel_id,
        channel_name: channel_names_by_id[channel_id].presence || param(row, :channel_name).to_s.strip.presence,
        upload_enabled: false,
        download_enabled: false,
        history_enabled: false
      }
      attrs[:channel_name] ||= channel_names_by_id[channel_id].presence || param(row, :channel_name).to_s.strip.presence
      attrs[:upload_enabled] ||= upload_enabled
      attrs[:download_enabled] ||= download_enabled
      attrs[:history_enabled] ||= history_enabled
    end
    seen.values
  end

  def as_claim_json
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
    Principal.bump_sync_config_cache_versions(principal_id)
  end
end
