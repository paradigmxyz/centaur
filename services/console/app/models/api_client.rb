require "digest"

# A machine client of the api-rs session API, such as an eval runner. api-rs
# exchanges the client id (the oid) and secret for a short-lived access token
# and runs the client's sessions as one of its attached principals. Console
# owns these rows; api-rs only reads them.
class ApiClient < ApplicationRecord
  oid_prefix "acl"

  SECRET_PREFIX = "acs_".freeze
  SCOPES = %w[sessions:read sessions:write].freeze

  # The plaintext secret, set only on the instance that issued it.
  attr_reader :secret

  belongs_to :created_by, class_name: "User"
  has_many :api_client_principals, dependent: :destroy
  has_many :principals, through: :api_client_principals

  validates :name, presence: true
  validates :secret_digest, presence: true, uniqueness: true
  validate :scopes_supported

  before_validation :issue_secret, on: :create

  def self.digest_secret(secret)
    Digest::SHA256.hexdigest(secret.to_s)
  end

  def rotate_secret!
    issue_secret(force: true)
    save!
  end

  def disabled?
    disabled_at.present?
  end

  def default_principal_link
    api_client_principals.find_by(default: true)
  end

  # Attaches `principal`, making it the default when it is the first one.
  def attach_principal!(principal)
    transaction do
      link = api_client_principals.find_or_initialize_by(principal: principal)
      link.default = true if api_client_principals.where(default: true).none?
      link.save!
      link
    end
  end

  def make_default!(link)
    transaction do
      api_client_principals.where(default: true).where.not(id: link.id).update_all(default: false)
      link.update!(default: true)
    end
  end

  private

  def issue_secret(force: false)
    return if secret_digest.present? && !force
    @secret = "#{SECRET_PREFIX}#{SecureRandom.hex(32)}"
    self.secret_digest = self.class.digest_secret(@secret)
  end

  def scopes_supported
    return errors.add(:scopes, "must be an array") unless scopes.is_a?(Array)
    errors.add(:scopes, "must include at least one scope") if scopes.empty?
    unsupported = scopes - SCOPES
    errors.add(:scopes, "contains unsupported values: #{unsupported.join(', ')}") if unsupported.any?
  end
end
