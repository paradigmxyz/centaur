# An aws_auth transform: re-signs matching outbound requests with AWS SigV4. The
# tool's AWS SDK signs each request with throwaway placeholder credentials; the
# proxy strips that signature and re-signs with the real credentials resolved
# from the access_key_id / secret_access_key (and optional session_token) secret
# sources, so the real keys never reach the workload. allowed_regions /
# allowed_services scope what the proxy will sign for. Each credential is its own
# secret source (role_kind credential_field, role = access_key_id /
# secret_access_key / session_token), mirroring hmac_secret; like gcp_auth and
# hmac, Principal#sync_transforms emits one transform per granted secret.
class AwsAuthSecret < ApplicationRecord
  oid_prefix "aas"

  include ForeignIdCollisionGuard

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  # Allowed credential roles. access_key_id + secret_access_key are mandatory;
  # session_token is optional (for temporary/STS credentials).
  CREDENTIAL_ROLES = %w[access_key_id secret_access_key session_token].freeze
  REQUIRED_CREDENTIALS = %w[access_key_id secret_access_key].freeze

  has_many :sources, class_name: "SecretSource", dependent: :destroy
  has_many :rules, class_name: "RequestRule", dependent: :destroy
  has_many :grants, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  # Maps to one entry in the iron-proxy `transforms` array as an aws_auth
  # transform. allowed_regions / allowed_services are omitted when empty so they
  # decode as the proxy's omitempty default (no scoping).
  def to_proxy_transform
    by_role = sources.index_by(&:role)
    config = {}
    config["access_key_id"] = by_role["access_key_id"].to_proxy_source if by_role["access_key_id"]
    config["secret_access_key"] = by_role["secret_access_key"].to_proxy_source if by_role["secret_access_key"]
    config["session_token"] = by_role["session_token"].to_proxy_source if by_role["session_token"]
    config["allowed_regions"] = allowed_regions if allowed_regions.present?
    config["allowed_services"] = allowed_services if allowed_services.present?
    config["rules"] = rules.map(&:to_proxy_rule)
    { "name" => "aws_auth", "config" => config }
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validate :labels_is_a_hash
  validate :allowed_regions_are_strings
  validate :allowed_services_are_strings
  validate :credential_roles_valid
  validate :at_least_one_rule

  private

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def allowed_regions_are_strings
    errors.add(:allowed_regions, "must be an array") unless allowed_regions.is_a?(Array)
    return unless allowed_regions.is_a?(Array)
    errors.add(:allowed_regions, "must all be strings") unless allowed_regions.all? { |r| r.is_a?(String) && r.present? }
  end

  def allowed_services_are_strings
    errors.add(:allowed_services, "must be an array") unless allowed_services.is_a?(Array)
    return unless allowed_services.is_a?(Array)
    errors.add(:allowed_services, "must all be strings") unless allowed_services.all? { |s| s.is_a?(String) && s.present? }
  end

  # Exactly the known credential roles, no duplicates, with the required ones
  # present. The (owner, role, kind) unique index also enforces no duplicates at
  # the DB level.
  def credential_roles_valid
    roles = sources.map(&:role)
    unknown = roles.compact.uniq - CREDENTIAL_ROLES
    errors.add(:sources, "has unknown credential role(s): #{unknown.join(", ")}") if unknown.any?
    errors.add(:sources, "has duplicate credential roles") if roles.length != roles.uniq.length
    (REQUIRED_CREDENTIALS - roles).each do |missing|
      errors.add(:sources, "is missing the required #{missing.inspect} credential")
    end
  end

  def at_least_one_rule
    errors.add(:rules, "must include at least one rule") if rules.empty?
  end
end
