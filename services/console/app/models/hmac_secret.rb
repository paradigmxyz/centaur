# An hmac_sign transform: signs matching outbound requests with an HMAC over a
# templated message and injects the signature (and any companion values) as
# request headers. The HMAC key, and any additional named credentials, are each
# their own secret source (role_kind credential_field, role = the credential
# name). Unlike oauth_token, the proxy carries each entry as its own hmac_sign
# transform with its own rules, so Principal#sync_transforms emits one transform
# per granted secret (like gcp_auth).
class HmacSecret < ApplicationRecord
  oid_prefix "hms"

  include ForeignIdCollisionGuard
  include SyncConfigCacheInvalidation

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  # Allowed values mirror iron-proxy's hmac_sign transform.
  TIMESTAMP_FORMATS = %w[unix_seconds unix_millis unix_nanos rfc3339].freeze
  ALGORITHMS = %w[sha256 sha512 sha1].freeze
  KEY_ENCODINGS = %w[raw base64 hex].freeze
  OUTPUT_ENCODINGS = %w[base64 hex].freeze

  # The HMAC key credential is mandatory; additional named credentials are
  # optional and available to templates as .Credentials.<name>.
  REQUIRED_CREDENTIAL = "secret".freeze

  has_many :sources, class_name: "SecretSource", dependent: :destroy
  has_many :rules, class_name: "RequestRule", dependent: :destroy
  has_many :grants, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  # Maps to one entry in the iron-proxy `transforms` array as an hmac_sign
  # transform. allow_chunked_body is omitted when false so it decodes as the
  # proxy's omitempty default.
  def to_proxy_transform
    config = {
      "credentials" => sources.to_h { |s| [ s.role, s.to_proxy_source ] },
      "timestamp" => { "format" => timestamp_format },
      "signature" => {
        "algorithm" => signature_algorithm,
        "key_encoding" => signature_key_encoding,
        "output_encoding" => signature_output_encoding,
        "message" => signature_message
      },
      "headers" => headers.map { |h| { "name" => h["name"], "value" => h["value"] } },
      "rules" => rules.map(&:to_proxy_rule)
    }
    config["allow_chunked_body"] = true if allow_chunked_body
    { "name" => "hmac_sign", "config" => config }
  end

  # hmac_sign injects its signature (and companion values) into the named request
  # headers; used for cross-type conflict detection in Principal#served_credentials.
  def proxy_conflict_targets
    headers.map { |h| "header:#{h["name"].downcase}" }
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validates :timestamp_format, inclusion: { in: TIMESTAMP_FORMATS, message: "must be one of #{TIMESTAMP_FORMATS.join(", ")}" }
  validates :signature_algorithm, inclusion: { in: ALGORITHMS, message: "must be one of #{ALGORITHMS.join(", ")}" }
  validates :signature_key_encoding, inclusion: { in: KEY_ENCODINGS, message: "must be one of #{KEY_ENCODINGS.join(", ")}" }
  validates :signature_output_encoding, inclusion: { in: OUTPUT_ENCODINGS, message: "must be one of #{OUTPUT_ENCODINGS.join(", ")}" }
  validates :signature_message, presence: true
  validate :labels_is_a_hash
  validate :headers_are_valid
  validate :credential_secret_present
  validate :at_least_one_rule

  private

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def headers_are_valid
    unless headers.is_a?(Array)
      errors.add(:headers, "must be an array")
      return
    end
    if headers.empty?
      errors.add(:headers, "must include at least one header")
      return
    end
    headers.each do |h|
      next if h.is_a?(Hash) && h["name"].is_a?(String) && h["name"].present? &&
              h["value"].is_a?(String) && h["value"].present?
      errors.add(:headers, "each header must have a non-blank name and value")
    end
  end

  def credential_secret_present
    roles = sources.map(&:role)
    roles.each do |role|
      errors.add(:sources, "credential name can't be blank") if role.blank?
    end
    errors.add(:sources, "is missing the required #{REQUIRED_CREDENTIAL.inspect} credential") unless roles.include?(REQUIRED_CREDENTIAL)
  end

  def at_least_one_rule
    errors.add(:rules, "must include at least one rule") if rules.empty?
  end
end
