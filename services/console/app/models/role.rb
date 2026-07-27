class Role < ApplicationRecord
  oid_prefix "role"

  include ForeignIdCollisionGuard

  attr_readonly :namespace, :foreign_id

  has_many :grants, dependent: :destroy
  has_many :principal_roles, dependent: :destroy
  has_many :principals, through: :principal_roles
  has_many :slack_channel_permissions, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  accepts_nested_attributes_for :slack_channel_permissions,
                                allow_destroy: true,
                                reject_if: :reject_slack_channel_permission_attributes?

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validate :labels_is_a_hash

  def slack_channel_permissions_payload
    permissions = if association(:slack_channel_permissions).loaded?
      slack_channel_permissions.sort_by { |permission| [ permission.channel_id, permission.id ] }
    else
      slack_channel_permissions.ordered
    end
    permissions.map(&:as_permission_json)
  end

  private

  def reject_slack_channel_permission_attributes?(attributes)
    attributes["channel_id"].blank?
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end
end
