require "uri"

module Oauth
  module Presets
    # Registers a tenant's Runlayer Plugin as an owner-only custom app. The
    # admin enters the tenant's MCP URL; the OAuth endpoints are its same-origin
    # /api/v1/oauth/* paths. Each user then connects their own account, which
    # serves only them, in DMs and channels (always_available).
    class Runlayer
      KEY = "runlayer"
      SCOPE = "mcp:proxy"
      # The `runlayer` sandbox CLI looks the app up by this slug.
      SLUG = KEY

      def key = KEY
      def display_name = "Runlayer"
      def token_endpoint_auth_method = "client_secret_post"

      def preset
        { slug: SLUG, description: "Your Runlayer tools and permissions.", allowed_scopes: [ SCOPE ],
          provider: Oauth::Providers::Custom::KEY, shared: false, owner_only: true, always_available: true,
          labels: { "centaur-tool" => KEY } }
      end

      # What the admin enters on the Integrations card.
      def setup_inputs
        [ { name: "resource_url", label: "MCP URL", type: "url", placeholder: "https://<tenant>/mcp" } ]
      end

      # The provider_config and registration endpoint for the MCP URL the admin
      # entered, or nil unless it is an exact HTTPS URL ending in /mcp.
      def setup_from(input)
        raw = input["resource_url"].to_s.strip
        uri = URI.parse(raw)
        return nil unless uri.is_a?(URI::HTTPS) && uri.host.present? && uri.port == 443 && uri.userinfo.nil? &&
          uri.query.nil? && uri.fragment.nil? && uri.path.end_with?("/mcp") && !raw.match?(/[*\[\]{}]/)

        endpoint = ->(action) { uri.dup.tap { |u| u.path = "/api/v1/oauth/#{action}" }.to_s }
        { provider_config: { "authorization_endpoint" => endpoint.call("authorize"), "token_endpoint" => endpoint.call("token"),
                             "revocation_endpoint" => endpoint.call("revoke"), "resource" => raw },
          registration_endpoint: endpoint.call("register") }
      rescue URI::InvalidURIError
        nil
      end
    end
  end
end
