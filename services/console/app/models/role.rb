class Role < ApplicationRecord
  oid_prefix "role"

  include ForeignIdCollisionGuard

  attr_readonly :namespace, :foreign_id

  has_many :grants, dependent: :destroy
  has_many :principal_roles, dependent: :destroy
  has_many :principals, through: :principal_roles
  has_many :slack_channel_permissions, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  include SlackChannelPermissionOwner

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validate :labels_is_a_hash

  def self.ensure_default_infra!(created_by:)
    role = find_or_initialize_by(namespace: "default", foreign_id: "infra")
    if role.new_record?
      role.assign_attributes(
        name: "Infra",
        labels: { "managed-by" => "centaur" },
        assign_by_default: true,
        created_by: created_by
      )
      role.save!
    elsif !role.assign_by_default?
      role.update!(assign_by_default: true)
    end
    role
  end

  def self.replace_default_assignments!(role_ids)
    transaction do
      now = Time.current
      where(assign_by_default: true).where.not(id: role_ids)
        .update_all(assign_by_default: false, updated_at: now)
      where(id: role_ids, assign_by_default: false)
        .update_all(assign_by_default: true, updated_at: now)
    end
  end

  private

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end
end
