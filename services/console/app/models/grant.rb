class Grant < ApplicationRecord
  oid_prefix "grant"

  include SyncConfigCacheInvalidation

  GRANTEE_ASSOCIATIONS = %i[principal role].freeze
  GRANTABLE_ASSOCIATIONS = %i[
    static_secret gcp_auth_secret gcp_id_token_secret aws_auth_secret oauth_token_secret pg_dsn_secret hmac_secret
  ].freeze

  # Higher priority wins. When two granted secrets collide at the proxy (iron-proxy
  # applies the last matching transform), the one with the higher priority is
  # emitted last and takes effect. Direct grants outrank role grants by default;
  # the wide gap leaves room to interleave explicit priorities later. Priority is
  # mutable, so operators can promote a role grant above a direct one when needed.
  DEFAULT_DIRECT_PRIORITY = 100
  DEFAULT_ROLE_PRIORITY = 0

  attr_readonly :principal_id, :role_id, :static_secret_id, :gcp_auth_secret_id,
                :gcp_id_token_secret_id, :aws_auth_secret_id, :oauth_token_secret_id,
                :pg_dsn_secret_id, :hmac_secret_id

  belongs_to :principal, optional: true
  belongs_to :role, optional: true
  belongs_to :static_secret, optional: true
  belongs_to :gcp_auth_secret, optional: true
  belongs_to :gcp_id_token_secret, optional: true
  belongs_to :aws_auth_secret, optional: true
  belongs_to :oauth_token_secret, optional: true
  belongs_to :pg_dsn_secret, optional: true
  belongs_to :hmac_secret, optional: true
  belongs_to :created_by, class_name: "User"

  before_validation :apply_default_priority, on: :create

  validate :exactly_one_grantee
  validate :exactly_one_grantable
  validate :role_grant_same_namespace
  validates :priority, presence: true, numericality: { only_integer: true }

  # The grantee this grant attaches the secret to: a principal or a role.
  def grantee
    GRANTEE_ASSOCIATIONS.filter_map { |assoc| send(assoc) }.first
  end

  # The granted credential, whichever type it is.
  def grantable
    GRANTABLE_ASSOCIATIONS.filter_map { |assoc| send(assoc) }.first
  end

  private

  def sync_config_affected_principal_ids
    ids = []
    ids << principal_id if principal_id.present?
    ids += PrincipalRole.where(role_id: role_id).pluck(:principal_id) if role_id.present?
    ids
  end

  # A grant left without an explicit priority defaults by grantee: direct grants
  # outrank role grants. The column is NOT NULL with no DB default, so Grant.new
  # leaves it nil and this hook fills it in before validation.
  def apply_default_priority
    return unless priority.nil?
    direct = principal_id.present? || principal.present?
    self.priority = direct ? DEFAULT_DIRECT_PRIORITY : DEFAULT_ROLE_PRIORITY
  end

  def exactly_one_grantee
    set = GRANTEE_ASSOCIATIONS.count { |assoc| send(assoc).present? }
    return if set == 1
    errors.add(:base, "must reference exactly one of #{GRANTEE_ASSOCIATIONS.join(", ")}")
  end

  def exactly_one_grantable
    set = GRANTABLE_ASSOCIATIONS.count { |assoc| send(assoc).present? }
    return if set == 1
    errors.add(:base, "must reference exactly one of #{GRANTABLE_ASSOCIATIONS.join(", ")}")
  end

  def role_grant_same_namespace
    return unless role.present?
    secret = grantable
    return unless secret.present?
    errors.add(:role, "must be in the same namespace as the secret") if role.namespace != secret.namespace
  end
end
