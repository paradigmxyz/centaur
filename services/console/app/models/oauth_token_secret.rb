# An oauth_token transform entry: mints OAuth2 access tokens for a single grant
# and injects them as a bearer header on matching requests. Each credential
# field (client_id, refresh_token, private_key, ...) is its own secret source,
# as are any token-endpoint headers. The proxy carries a list of these entries
# under one oauth_token transform; Proxy#sync_transforms bundles all of a
# proxy's granted entries together.
class OauthTokenSecret < ApplicationRecord
  oid_prefix "ots"

  include ForeignIdCollisionGuard

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  # Required/optional credential field roles per grant, mirroring iron-proxy's
  # internal/transform/oauth buildCredentialSources.
  GRANT_FIELDS = {
    "refresh_token"      => { required: %w[refresh_token client_id], optional: %w[client_secret] },
    "client_credentials" => { required: %w[client_id client_secret], optional: %w[] },
    "password"           => { required: %w[username password client_id], optional: %w[client_secret] },
    "jwt_bearer"         => { required: %w[issuer subject private_key], optional: %w[private_key_id] }
  }.freeze

  GRANTS = GRANT_FIELDS.keys.freeze
  CREDENTIAL_FIELDS = GRANT_FIELDS.values.flat_map { |s| s[:required] + s[:optional] }.uniq.freeze

  has_many :sources, class_name: "SecretSource", dependent: :destroy
  has_many :rules, class_name: "RequestRule", dependent: :destroy
  has_many :grants, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  # Maps to one entry in the iron-proxy oauth_token transform's `tokens` list.
  def to_proxy_entry
    entry = { "grant" => grant, "token_endpoint" => token_endpoint }
    sources.select(&:credential_field?).each { |s| entry[s.role] = s.to_proxy_source }
    entry["audience"] = audience if audience.present?
    entry["scopes"] = scopes if scopes.present?
    entry["header"] = header if header.present?
    entry["value_prefix"] = value_prefix if value_prefix.present?

    headers = sources.select(&:endpoint_header?)
    entry["token_endpoint_headers"] = headers.to_h { |s| [ s.role, s.to_proxy_source ] } if headers.any?

    entry["rules"] = rules.map(&:to_proxy_rule)
    entry
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validates :grant, inclusion: { in: GRANTS, message: "must be one of #{GRANTS.join(", ")}" }
  validates :token_endpoint, presence: true
  validate :labels_is_a_hash
  validate :audience_required_for_jwt_bearer
  validate :scopes_is_an_array
  validate :credential_sources_satisfy_grant
  validate :at_least_one_rule

  private

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def audience_required_for_jwt_bearer
    return unless grant == "jwt_bearer"
    errors.add(:audience, "can't be blank for the jwt_bearer grant") if audience.blank?
  end

  def scopes_is_an_array
    return if scopes.is_a?(Array) && scopes.all? { |s| s.is_a?(String) }
    errors.add(:scopes, "must be an array of strings")
  end

  def credential_sources_satisfy_grant
    spec = GRANT_FIELDS[grant]
    return unless spec # an invalid grant is reported separately

    credential_roles = sources.select(&:credential_field?).map(&:role)

    credential_roles.each do |role|
      errors.add(:sources, "#{role.inspect} is not a valid credential field") unless CREDENTIAL_FIELDS.include?(role)
    end

    (spec[:required] - credential_roles).each do |missing|
      errors.add(:sources, "is missing required field #{missing.inspect} for the #{grant} grant")
    end

    allowed = spec[:required] + spec[:optional]
    ((credential_roles & CREDENTIAL_FIELDS) - allowed).each do |unused|
      errors.add(:sources, "field #{unused.inspect} is not used by the #{grant} grant")
    end
  end

  def at_least_one_rule
    errors.add(:rules, "must include at least one rule") if rules.empty?
  end
end
