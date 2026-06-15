class PrincipalRole < ApplicationRecord
  oid_prefix "prole"

  belongs_to :principal
  belongs_to :role

  validates :role_id, uniqueness: { scope: :principal_id, message: "is already assigned to this principal" }
  validate :same_namespace

  private

  # A principal may only hold roles from its own namespace; roles are scoped to
  # a namespace and crossing that boundary would leak secrets across tenants.
  def same_namespace
    return if principal.nil? || role.nil?
    errors.add(:role, "must be in the same namespace as the principal") if principal.namespace != role.namespace
  end
end
