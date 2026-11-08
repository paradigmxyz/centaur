require "base64"
require "json"
require "uri"

module Oauth
  module Providers
    # Operator-configured provider for authorization servers that need no
    # provider-specific code: the authorization-code flow with PKCE and a
    # confidential client, an RFC 8707 resource indicator naming the protected
    # resource on the authorization, token, and refresh requests (as MCP servers
    # require), and the account identity from a userinfo endpoint or, when the
    # server has none, from the claims of a JWT access token. The endpoints live
    # in the OAuth app's provider_config, so OauthApp#provider_strategy builds a
    # configured instance per app; the registry's unconfigured instance only
    # names the provider.
    #
    # The minted token is injected only for requests to the resource URL's host
    # and path.
    #
    # SECURITY: access-token claims are decoded without verifying the signature.
    # The token came directly from the configured token endpoint over TLS, so
    # its claims are trusted as much as a userinfo response would be.
    class Custom
      include HttpIdentity

      KEY = "custom"
      CONFIG_KEYS = %w[authorization_endpoint token_endpoint userinfo_endpoint resource].freeze
      OPTIONAL_CONFIG_KEYS = %w[userinfo_endpoint].freeze

      # Validation messages for a provider_config hash; empty when valid.
      def self.config_errors(config)
        unknown = config.keys - CONFIG_KEYS
        errors = unknown.any? ? [ "has unknown keys: #{unknown.join(', ')}" ] : []
        CONFIG_KEYS.each_with_object(errors) do |key, messages|
          next if OPTIONAL_CONFIG_KEYS.include?(key) && config[key].blank?

          messages << "#{key} must be an https URL" unless https_url?(config[key])
        end
      end

      def self.https_url?(value)
        uri = URI.parse(value.to_s)
        uri.is_a?(URI::HTTPS) && uri.host.present? && uri.userinfo.nil?
      rescue URI::InvalidURIError
        false
      end

      def initialize(app = nil)
        @app = app
        @config = app&.provider_config || {}
      end

      def key = KEY
      def display_name = @app&.slug || "Custom"
      def authorization_endpoint = @config["authorization_endpoint"]
      def token_endpoint = @config["token_endpoint"]
      def identity_scopes = []
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = { "resource" => @config["resource"] }
      def extra_token_params = { "resource" => @config["resource"] }
      def refreshable? = true
      def parse_granted_scopes(scope) = scope.to_s.split
      # A refresh without a scope keeps the granted set (RFC 6749 section 6).
      def refresh_scopes(_scopes) = []

      def credential_request_rules
        resource = URI.parse(@config["resource"])
        path = resource.path
        [ { host: resource.host, http_methods: [], paths: path.blank? || path == "/" ? [] : [ path ] } ]
      end

      def identity_from(result, client_id:, http_client: HttpClient.new)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        claims = if @config["userinfo_endpoint"].present?
          userinfo_claims(result.access_token, http_client)
        else
          access_token_claims(result.access_token)
        end
        subject = require_identity(claims["sub"], provider: display_name).to_s
        { subject: subject, email: claims["email"].presence, name: claims["name"].presence }
      end

      private

      def userinfo_claims(access_token, http_client)
        response = identity_response(provider: display_name) do
          http_client.get(@config["userinfo_endpoint"], headers: {
            "Authorization" => "Bearer #{access_token}", "Accept" => "application/json"
          })
        end
        identity_json(response, provider: display_name)
      end

      # The payload of a JWT access token (the second segment, unpadded
      # base64url).
      def access_token_claims(access_token)
        segment = access_token.split(".")[1].to_s
        segment += "=" * ((4 - segment.length % 4) % 4)
        claims = JSON.parse(Base64.urlsafe_decode64(segment))
        raise JSON::ParserError unless claims.is_a?(Hash)

        claims
      rescue ArgumentError, JSON::ParserError
        raise Broker::ExchangeError.new("access token is not a JWT and no userinfo endpoint is configured",
                                        stage: "parse", code: "invalid_identity_response")
      end
    end
  end
end
