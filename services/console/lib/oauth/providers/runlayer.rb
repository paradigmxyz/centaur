require "uri"

module Oauth
  module Providers
    # Runlayer's user OAuth client for the unified Plugin protected resource.
    # Credentials belong to the authenticated Console user. MCP OAuth does not
    # promise OIDC/userinfo, so we neither decode access tokens nor infer an
    # external account from email. Reconnecting replaces this owner's grant.
    class Runlayer
      KEY = "runlayer"
      SCOPE = "mcp:proxy"

      def key = KEY
      def display_name = "Runlayer"
      def configured? = resource_uri.present?
      def resource_url = configured? ? ENV["RUNLAYER_MCP_URL"].to_s.strip : nil
      def authorization_endpoint = endpoint("authorize")
      def token_endpoint = endpoint("token")
      def registration_endpoint = endpoint("register")
      def revocation_endpoint = endpoint("revoke")
      def token_endpoint_auth_method = "client_secret_post"
      def identity_scopes = []
      def required_scopes = [ SCOPE ]
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = { "resource" => resource_url }
      def refreshable? = true
      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(scopes) = Array(scopes)
      def consent_context = resource_url
      def api_hosts = [ resource_uri!.host ]

      def credential_request_rules
        [ { host: resource_uri!.host, http_methods: %w[GET POST DELETE], paths: [ resource_uri!.path ] } ]
      end

      def preset
        { slug: KEY, description: "Your Runlayer tools and permissions.",
          allowed_scopes: [ SCOPE ], shared: false, always_available: true }
      end

      def identity_for_user(user)
        { subject: "owner-#{user.oid}", name: "Your Runlayer connection",
          labels: { "runlayer_resource" => resource_url } }
      end

      def wrapping_secret_labels = { "centaur-tool" => KEY }

      def validate_result!(result)
        return if result.scope.blank? || parse_granted_scopes(result.scope).include?(SCOPE)

        raise Broker::ExchangeError.new("Runlayer scope was not granted", stage: "oauth", code: "missing_scope")
      end

      def refresh_resource(credential)
        unless configured?
          raise Broker::RefreshError.new("Runlayer configuration is unavailable; refresh deferred", stage: "config", code: "configuration_unavailable", retryable: true)
        end

        unless credential_resource_matches?(credential)
          raise Broker::RefreshError.new("Runlayer configuration changed; reconnect", stage: "oauth", code: "invalid_grant", retryable: false)
        end
        resource_url
      end

      def credential_resource_matches?(credential)
        configured? && credential.labels["runlayer_resource"] == resource_url && credential.token_endpoint == token_endpoint
      end

      private

      def resource_uri
        raw = ENV["RUNLAYER_MCP_URL"].to_s.strip
        return nil if raw.empty?

        uri = URI.parse(raw)
        return nil unless uri.is_a?(URI::HTTPS) && uri.host.present? && uri.port == 443 &&
          uri.userinfo.nil? && uri.query.nil? && uri.fragment.nil? && uri.path.end_with?("/mcp") &&
          !uri.path.match?(/[?*\[\]{}]/)

        uri
      rescue URI::InvalidURIError
        nil
      end

      def resource_uri!
        resource_uri || raise(Broker::ExchangeError.new("Set RUNLAYER_MCP_URL to an exact HTTPS Runlayer MCP URL", stage: "config"))
      end

      def endpoint(action)
        uri = resource_uri!.dup
        uri.path = "/api/v1/oauth/#{action}"
        uri.to_s
      end
    end
  end
end
