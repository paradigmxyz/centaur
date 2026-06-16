# A gcp_auth transform: mints short-lived GCP OAuth2 access tokens and injects
# them as Authorization: Bearer on matching requests. Credentials come from
# either a nested keyfile secret source or Application Default Credentials
# (credentials_provider). centaur-console deliberately does not expose the proxy's
# keyfile_path option, which points at the proxy host's local disk.
class GcpAuthSecret < ApplicationRecord
  oid_prefix "gas"

  include ForeignIdCollisionGuard
  include SyncConfigCacheInvalidation

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  CREDENTIALS_PROVIDER_SCHEMA = JSONSchemer.schema({
    "type" => "object",
    "additionalProperties" => false,
    "required" => [ "type" ],
    "properties" => {
      "type" => { "enum" => [ "workload_identity" ] }
    }
  })

  has_one :keyfile_source, class_name: "SecretSource", dependent: :destroy
  has_many :rules, class_name: "RequestRule", dependent: :destroy
  has_many :grants, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  # Maps to one entry in the iron-proxy `transforms` array as a gcp_auth
  # transform. Exactly one credential source (keyfile or credentials_provider)
  # is present, enforced by validation.
  def to_proxy_transform
    config = {}
    config["keyfile"] = keyfile_source.to_proxy_source if keyfile_source
    config["credentials_provider"] = credentials_provider if credentials_provider.present?
    config["subject"] = subject if subject.present?
    config["scopes"] = scopes
    config["rules"] = rules.map(&:to_proxy_rule)
    { "name" => "gcp_auth", "config" => config }
  end

  # gcp_auth always sets the Authorization header (a Bearer access token); used
  # for cross-type conflict detection in Principal#served_credentials.
  def proxy_conflict_targets
    [ "header:authorization" ]
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validate :labels_is_a_hash
  validate :exactly_one_credential
  validate :scopes_are_present_strings
  validate :subject_only_with_keyfile
  validate :credentials_provider_matches_schema

  private

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def exactly_one_credential
    present = [ keyfile_source.present?, credentials_provider.present? ].count(true)
    if present.zero?
      errors.add(:base, "must define one of keyfile (source) or credentials_provider")
    elsif present > 1
      errors.add(:base, "keyfile and credentials_provider are mutually exclusive")
    end
  end

  def scopes_are_present_strings
    unless scopes.is_a?(Array)
      errors.add(:scopes, "must be an array")
      return
    end
    if scopes.empty?
      errors.add(:scopes, "must include at least one scope")
      return
    end
    errors.add(:scopes, "must all be strings") unless scopes.all? { |s| s.is_a?(String) && s.present? }
  end

  # Mirrors the proxy: domain-wide delegation (subject) only works with a
  # keyfile; metadata-server credentials cannot impersonate.
  def subject_only_with_keyfile
    return if subject.blank?
    errors.add(:subject, "is only allowed with a keyfile source") if credentials_provider.present?
  end

  def credentials_provider_matches_schema
    return if credentials_provider.blank?
    unless credentials_provider.is_a?(Hash)
      errors.add(:credentials_provider, "must be a hash")
      return
    end
    CREDENTIALS_PROVIDER_SCHEMA.validate(credentials_provider).each do |err|
      pointer = err["data_pointer"].presence || "(root)"
      errors.add(:credentials_provider, "#{pointer} #{err["error"]}")
    end
  end
end
