module Oauth
  module Providers
    # Zoom user-managed OAuth. Zoom requires
    # client_secret_basic for both code exchange and refresh and rotates refresh
    # tokens, which the broker persists under its single-writer row lock.
    class Zoom
      include HttpIdentity

      KEY = "zoom"
      AUTHORIZATION_ENDPOINT = "https://zoom.us/oauth/authorize"
      TOKEN_ENDPOINT = "https://zoom.us/oauth/token"
      SELF_ENDPOINT = "https://api.zoom.us/v2/users/me"
      IDENTITY_SCOPES = [].freeze
      REQUIRED_SCOPES = %w[user:read:user].freeze
      API_HOSTS = %w[api.zoom.us].freeze

      def key = KEY
      def display_name = "Zoom"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def required_scopes = REQUIRED_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = {}
      def refreshable? = true
      def token_endpoint_auth_method = "client_secret_basic"

      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(_scopes) = []
      def foreign_id_subject(subject) = subject.to_s

      def identity_from(result, client_id:, http_client: HttpClient.new)
        response = identity_response(provider: display_name) do
          http_client.get(
            SELF_ENDPOINT,
            headers: {
              "Authorization" => "Bearer #{result.access_token}",
              "User-Agent" => "centaur-console"
            }
          )
        end
        user = identity_json(response, provider: display_name)
        subject = require_identity(user["id"], provider: display_name).to_s
        {
          subject: subject,
          email: user["email"].presence,
          name: user["display_name"].presence || user["email"].presence || subject
        }
      end
    end
  end
end
