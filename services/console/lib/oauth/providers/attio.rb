module Oauth
  module Providers
    # Attio OAuth consent-flow strategy. Attio app scopes are configured in the
    # Attio developer dashboard, not requested on the authorization redirect;
    # the token response carries a long-lived workspace-scoped access token with
    # no refresh token, expiry, scope, or identity payload. Resolve /v2/self
    # synchronously so the callback upserts by the stable workspace id.
    class Attio
      include HttpIdentity

      KEY = "attio"
      AUTHORIZATION_ENDPOINT = "https://app.attio.com/authorize"
      TOKEN_ENDPOINT = "https://app.attio.com/oauth/token"
      SELF_ENDPOINT = "https://api.attio.com/v2/self"
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.attio.com].freeze

      def key = KEY
      def display_name = "Attio"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = {}
      def refreshable? = false

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(_scopes) = []

      def identity_from(result, client_id:, http_client: HttpClient.new)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        response = identity_response(provider: display_name) do
          http_client.get(
            SELF_ENDPOINT,
            headers: {
              "Authorization" => "Bearer #{result.access_token}",
              "User-Agent" => "centaur-console"
            }
          )
        end
        workspace = identity_json(response, provider: display_name)
        subject = require_identity(workspace["workspace_id"], provider: display_name).to_s

        {
          subject: subject,
          email: nil,
          name: workspace["workspace_name"].presence || workspace["workspace_slug"].presence || subject
        }
      end
    end
  end
end
