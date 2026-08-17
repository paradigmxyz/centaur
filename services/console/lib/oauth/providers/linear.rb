module Oauth
  module Providers
    # Linear OAuth consent-flow strategy. Linear's token response carries the
    # access token, granted scopes, and rotating refresh token but no account
    # identity. Resolve the authenticated viewer synchronously so the callback
    # can upsert by the stable Linear user id.
    class Linear
      include HttpIdentity

      KEY = "linear"
      AUTHORIZATION_ENDPOINT = "https://linear.app/oauth/authorize"
      TOKEN_ENDPOINT = "https://api.linear.app/oauth/token"
      GRAPHQL_ENDPOINT = "https://api.linear.app/graphql"
      VIEWER_QUERY = "{ viewer { id name email } }".freeze
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.linear.app].freeze

      def key = KEY
      def display_name = "Linear"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = ","
      def extra_authorization_params = {}
      def refreshable? = true

      def parse_granted_scopes(scope)
        case scope
        when Array
          scope.map(&:to_s).reject(&:blank?)
        else
          scope.to_s.split(/[,\s]+/).reject(&:blank?)
        end
      end

      # Linear accepts an optional scope parameter on refresh. Pass through the
      # originally granted scopes so a refresh preserves the consented grant set;
      # CredentialGrants serializes this as the token endpoint's space-separated
      # scope field.
      def refresh_scopes(scopes) = Array(scopes)

      def identity_from(result, client_id:, http_client: HttpClient.new)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        response = identity_response(provider: display_name) do
          http_client.post(
            GRAPHQL_ENDPOINT,
            json: { query: VIEWER_QUERY },
            headers: {
              "Authorization" => "Bearer #{result.access_token}",
              "User-Agent" => "centaur-console"
            }
          )
        end
        payload = identity_json(response, provider: display_name)
        viewer = payload.dig("data", "viewer")
        unless viewer.is_a?(Hash)
          raise Broker::ExchangeError.new(
            "Linear identity endpoint returned no viewer",
            stage: "parse",
            code: "missing_identity",
            status: response.status
          )
        end
        subject = require_identity(viewer["id"], provider: display_name).to_s

        {
          subject: subject,
          email: viewer["email"].presence,
          name: viewer["name"].presence || viewer["email"].presence || subject
        }
      end
    end
  end
end
