class SlackChannelPermission < ApplicationRecord
  attr_readonly :principal_id, :role_id

  belongs_to :principal, optional: true
  belongs_to :role, optional: true

  before_validation :normalize_channel_fields
  after_commit :bump_sync_config_cache_versions

  validates :channel_id, presence: true,
                         format: { with: Principal::SLACK_CHANNEL_ID_FORMAT, message: "is not a valid Slack channel ID" }
  validates :channel_id, uniqueness: { scope: :principal_id }, if: :principal_id?
  validates :channel_id, uniqueness: { scope: :role_id }, if: :role_id?
  validates :upload_enabled, inclusion: { in: [ true, false ] }
  validates :download_enabled, inclusion: { in: [ true, false ] }
  validates :history_enabled, inclusion: { in: [ true, false ] }
  validate :exactly_one_grantee
  validate :at_least_one_permission

  scope :ordered, -> { order(:channel_id, :id) }

  def self.replace_for_principal!(principal, permission_rows)
    replace_for!(principal.slack_channel_permissions, permission_rows)
  end

  def self.replace_for_role!(role, permission_rows)
    replace_for!(role.slack_channel_permissions, permission_rows)
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

  def self.replace_for!(association, permission_rows)
    rows_by_channel = permission_rows.each_with_object({}) do |raw_attrs, rows|
      attrs = raw_attrs.to_h.symbolize_keys
      channel_id = attrs[:channel_id].to_s.strip.upcase
      row = rows[channel_id] ||= {
        channel_id: channel_id,
        channel_name: nil,
        upload_enabled: false,
        download_enabled: false,
        history_enabled: false
      }
      row[:channel_name] ||= attrs[:channel_name].to_s.strip.presence
      %i[upload_enabled download_enabled history_enabled].each do |permission|
        row[permission] ||= ActiveModel::Type::Boolean.new.cast(attrs[permission]) == true
      end
    end

    transaction do
      association.destroy_all
      rows_by_channel.each_value { |attrs| association.create!(attrs) }
    end
  end
  private_class_method :replace_for!

  def normalize_channel_fields
    self.channel_id = channel_id.to_s.strip.upcase
    self.channel_name = channel_name.to_s.strip.presence
  end

  def at_least_one_permission
    return if upload_enabled || download_enabled || history_enabled
    errors.add(:base, "Select at least one Slack permission")
  end

  def exactly_one_grantee
    return if [ principal, role ].compact.one?

    errors.add(:base, "must reference exactly one of principal, role")
  end

  def bump_sync_config_cache_versions
    principal_ids = [ principal_id ]
    principal_ids.concat(PrincipalRole.where(role_id: role_id).pluck(:principal_id)) if role_id.present?
    Principal.bump_sync_config_cache_versions(principal_ids)
  end
end
