require "ipaddr"
require "uri"

class McpOauthClient < ApplicationRecord
  oid_prefix "moc"

  DEFAULT_GRANT_TYPES = %w[authorization_code refresh_token].freeze
  DEFAULT_RESPONSE_TYPES = %w[code].freeze
  DEFAULT_SCOPES = %w[mcp:tools].freeze

  has_many :authorization_codes, class_name: "McpOauthAuthorizationCode", dependent: :destroy
  has_many :refresh_tokens, class_name: "McpOauthRefreshToken", dependent: :destroy

  validates :redirect_uris, presence: true
  validate :redirect_uris_valid
  validate :grant_types_supported
  validate :response_types_supported
  validate :scopes_supported

  def public_client_id = oid

  def redirect_uri_allowed?(uri)
    return false unless self.class.allowed_redirect_uri?(uri)

    requested = URI.parse(uri.to_s)
    redirect_uris.any? do |registered|
      next false unless self.class.allowed_redirect_uri?(registered)
      next true if registered == uri.to_s

      registered_uri = URI.parse(registered.to_s)
      loopback_redirect_uri_match?(registered_uri, requested)
    rescue URI::InvalidURIError
      false
    end
  rescue URI::InvalidURIError
    false
  end

  private

  def redirect_uris_valid
    return errors.add(:redirect_uris, "must be an array") unless redirect_uris.is_a?(Array)
    errors.add(:redirect_uris, "must not be empty") if redirect_uris.empty?
    redirect_uris.each do |uri|
      errors.add(:redirect_uris, "#{uri.inspect} is not an allowed public-client redirect URI") unless self.class.allowed_redirect_uri?(uri)
    end
  end

  def grant_types_supported
    return errors.add(:grant_types, "must be an array") unless grant_types.is_a?(Array)
    unsupported = grant_types - DEFAULT_GRANT_TYPES
    errors.add(:grant_types, "contains unsupported values: #{unsupported.join(', ')}") if unsupported.any?
  end

  def response_types_supported
    return errors.add(:response_types, "must be an array") unless response_types.is_a?(Array)
    unsupported = response_types - DEFAULT_RESPONSE_TYPES
    errors.add(:response_types, "contains unsupported values: #{unsupported.join(', ')}") if unsupported.any?
  end

  def scopes_supported
    return errors.add(:scopes, "must be an array") unless scopes.is_a?(Array)
    unsupported = scopes - DEFAULT_SCOPES
    errors.add(:scopes, "contains unsupported values: #{unsupported.join(', ')}") if unsupported.any?
  end

  def self.allowed_redirect_uri?(value)
    uri = URI.parse(value.to_s)
    uri.scheme == "http" && loopback_host?(uri.host)
  rescue URI::InvalidURIError
    false
  end

  def loopback_redirect_uri_match?(registered_uri, requested_uri)
    return false unless registered_uri.scheme == "http" && requested_uri.scheme == "http"
    return false unless self.class.loopback_host?(registered_uri.host)
    return false unless self.class.loopback_host?(requested_uri.host)
    return false if registered_uri.port && registered_uri.port != registered_uri.default_port
    return false unless registered_uri.path == requested_uri.path
    return false unless registered_uri.query == requested_uri.query

    true
  end

  def self.loopback_host?(host)
    normalized = host.to_s.downcase
    return true if normalized == "localhost"

    IPAddr.new(normalized).loopback?
  rescue IPAddr::Error
    false
  end
end
