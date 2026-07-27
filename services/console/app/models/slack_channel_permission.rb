class SlackChannelPermission < ApplicationRecord
  include SyncConfigCacheInvalidation

  attr_readonly :principal_id, :role_id

  belongs_to :principal, optional: true
  belongs_to :role, optional: true

  before_validation :clear_stale_channel_name
  before_validation :normalize_channel_fields

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
    replace_for!(principal, permission_rows)
  end

  def self.replace_for_role!(role, permission_rows)
    replace_for!(role, permission_rows)
  end

  def self.replace_for!(grantee, permission_rows)
    association = grantee.slack_channel_permissions
    rows_by_channel = normalized_permission_rows(permission_rows)
    affected_principal_ids = principal_ids_for_grantee(grantee)

    transaction do
      association.delete_all
      association.reset
      records = rows_by_channel.map { |attrs| association.build(attrs) }
      records.each do |record|
        raise ActiveRecord::RecordInvalid, record unless record.valid?
      end

      now = Time.current
      insert_all!(records.map { |record| bulk_insert_attributes(record, now) }) if records.any?
      Principal.bump_sync_config_cache_versions(affected_principal_ids)
    end
  ensure
    association&.reset
  end

  def self.principal_ids_for_grantee(grantee)
    case grantee
    when Principal then [ grantee.id ]
    when Role then PrincipalRole.where(role_id: grantee.id).pluck(:principal_id)
    else []
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

  def self.normalized_permission_rows(permission_rows)
    permission_rows.each_with_object({}) do |raw_attrs, rows|
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
    end.values
  end
  private_class_method :normalized_permission_rows

  def self.bulk_insert_attributes(record, timestamp)
    record.attributes.slice(
      "principal_id",
      "role_id",
      "channel_id",
      "channel_name",
      "upload_enabled",
      "download_enabled",
      "history_enabled"
    ).merge("created_at" => timestamp, "updated_at" => timestamp)
  end
  private_class_method :bulk_insert_attributes

  def clear_stale_channel_name
    self.channel_name = nil if persisted? && will_save_change_to_channel_id?
  end

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

  def sync_config_affected_principal_ids
    self.class.principal_ids_for_grantee(principal || role)
  end
end
