require "uri"

module Oauth
  module Providers
    # Operator-configured provider for authorization servers that need no
    # provider-specific code: the authorization-code flow with PKCE and a
    # confidential client, an RFC 8707 resource indicator naming the protected
    # resource (as MCP servers require), and the account identity from a
    # userinfo endpoint. The endpoints live in the OAuth app's provider_config,
    # so OauthApp#provider_strategy builds a configured instance per app; the
    # registry's unconfigured instance only names the provider.
    #
    # The minted token is injected only for requests to the resource URL's host
    # and path.
    class Custom
      include HttpIdentity

      KEY = "custom"
      CONFIG_KEYS = %w[authorization_endpoint token_endpoint userinfo_endpoint resource].freeze

      # Validation messages for a provider_config hash; empty when valid.
      def self.config_errors(config)
        unknown = config.keys - CONFIG_KEYS
        errors = unknown.any? ? [ "has unknown keys: #{unknown.join(', ')}" ] : []
        CONFIG_KEYS.each_with_object(errors) do |key, messages|
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

        response = identity_response(provider: display_name) do
          http_client.get(@config["userinfo_endpoint"], headers: {
            "Authorization" => "Bearer #{result.access_token}", "Accept" => "application/json"
          })
        end
        claims = identity_json(response, provider: display_name)
        subject = require_identity(claims["sub"], provider: display_name).to_s
        { subject: subject, email: claims["email"].presence, name: claims["name"].presence }
      end
    end
  end
end
