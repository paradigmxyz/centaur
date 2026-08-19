class SlackChannelPermission < ApplicationRecord
  include SyncConfigCacheInvalidation

  oid_prefix "scp"

  PERMISSION_FLAGS = [
    { attribute: :upload_enabled, key: :upload, label: "Upload" },
    { attribute: :download_enabled, key: :download, label: "Download" },
    { attribute: :history_enabled, key: :history, label: "History" }
  ].freeze
  PERMISSION_ATTRIBUTES = PERMISSION_FLAGS.map { |flag| flag.fetch(:attribute) }.freeze
  PERMISSION_ATTRIBUTE_NAMES = PERMISSION_ATTRIBUTES.map(&:to_s).freeze
  DEFAULT_ENABLED_ATTRIBUTES = PERMISSION_ATTRIBUTES.to_h { |permission| [ permission, true ] }.freeze

  attr_readonly :principal_id, :role_id, :channel_id

  belongs_to :principal, optional: true
  belongs_to :role, optional: true

  before_validation :normalize_channel_id

  validates :channel_id, presence: true,
                         format: { with: Principal::SLACK_CHANNEL_ID_FORMAT, message: "is not a valid Slack channel ID" }
  validates :channel_id, uniqueness: { scope: :principal_id }, if: :principal_id?
  validates :channel_id, uniqueness: { scope: :role_id }, if: :role_id?
  PERMISSION_ATTRIBUTES.each do |permission|
    validates permission, inclusion: { in: [ true, false ] }
  end
  validate :exactly_one_grantee
  validate :at_least_one_permission

  scope :ordered, -> { order(:channel_id, :id) }

  def self.permission_rows_payload(permission_rows)
    normalized_permission_rows(permission_rows).map do |attrs|
      { "channel_id" => attrs.fetch(:channel_id) }.merge(
        PERMISSION_ATTRIBUTE_NAMES.to_h { |permission| [ permission, attrs.fetch(permission.to_sym) ] }
      )
    end.sort_by { |row| row.fetch("channel_id") }
  end

  def self.replace_for!(grantee, permission_rows)
    association = grantee.slack_channel_permissions
    rows_by_channel = normalized_permission_rows(permission_rows)
    affected_principals = principals_for_grantee(grantee)

    transaction do
      association.delete_all
      association.reset
      records = rows_by_channel.map { |attrs| association.build(attrs) }
      records.each do |record|
        raise ActiveRecord::RecordInvalid, record unless record.valid?
      end

      now = Time.current
      insert_all!(records.map { |record| bulk_insert_attributes(record, now) }) if records.any?
      Principal.bump_sync_config_cache_versions(affected_principals)
    end
  ensure
    grantee&.reset_slack_channel_permissions_cache! if grantee.respond_to?(:reset_slack_channel_permissions_cache!)
    association&.reset
  end

  def self.principals_for_grantee(grantee)
    case grantee
    when Principal then Principal.where(id: grantee.id)
    when Role then Principal.where(id: PrincipalRole.where(role_id: grantee.id).select(:principal_id))
    else Principal.none
    end
  end

  def as_permission_json
    { "channel_id" => channel_id }.merge(
      PERMISSION_ATTRIBUTE_NAMES.to_h { |permission| [ permission, public_send(permission) ] }
    )
  end

  private

  def self.normalized_permission_rows(permission_rows)
    permission_rows.each_with_object({}) do |raw_attrs, rows|
      attrs = raw_attrs.to_h.symbolize_keys
      channel_id = attrs[:channel_id].to_s.strip.upcase
      row = rows[channel_id] ||= {
        channel_id: channel_id
      }
      PERMISSION_ATTRIBUTES.each { |permission| row[permission] = false unless row.key?(permission) }
      PERMISSION_ATTRIBUTES.each do |permission|
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
      *PERMISSION_ATTRIBUTE_NAMES
    ).merge("created_at" => timestamp, "updated_at" => timestamp)
  end
  private_class_method :bulk_insert_attributes

  def normalize_channel_id
    self.channel_id = channel_id.to_s.strip.upcase if new_record?
  end

  def at_least_one_permission
    return if PERMISSION_ATTRIBUTES.any? { |permission| public_send(permission) }
    errors.add(:base, "Select at least one Slack permission")
  end

  def exactly_one_grantee
    return if [ principal, role ].compact.one?

    errors.add(:base, "must reference exactly one of principal, role")
  end

  def sync_config_affected_principals
    self.class.principals_for_grantee(principal || role)
  end
end
