module Oauth
  module Providers
    # Slack user-token consent-flow strategy. Uses Slack's user-centric OAuth v2
    # endpoints so the broker stores a single user token, with identity carried
    # by the returned OIDC id_token.
    class Slack
      KEY = "slack"
      AUTHORIZATION_ENDPOINT = "https://slack.com/oauth/v2_user/authorize"
      TOKEN_ENDPOINT = "https://slack.com/api/oauth.v2.user.access"
      # Do not add Sign in with Slack scopes here. Slack rejects requests that
      # mix SIWS scopes with normal API scopes such as channels:history.
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[slack.com].freeze
      VALID_ISSUERS = %w[https://slack.com].freeze

      def key = KEY
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def scope_separator = ","
      def extra_authorization_params = {}

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(_scopes) = []

      def identity_from(result, client_id:)
        user_id = result.response&.dig("authed_user", "id")
        if user_id.present?
          return {
            subject: user_id,
            email: result.response.dig("authed_user", "email")
          }
        end

        Login::IdToken.identity(result.id_token, client_id: client_id,
                                                 valid_issuers: VALID_ISSUERS)
                      .slice(:subject, :email)
      end
    end
  end
end
