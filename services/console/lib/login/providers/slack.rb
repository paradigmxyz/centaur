module Login
  module Providers
    # Slack "Sign in with Slack" (OpenID Connect) strategy for console login.
    # Slack's OIDC endpoints differ from its regular OAuth ones; the token
    # endpoint returns an id_token carrying the account identity.
    class Slack
      KEY = "slack"
      AUTHORIZATION_ENDPOINT = "https://slack.com/openid/connect/authorize"
      TOKEN_ENDPOINT = "https://slack.com/api/openid.connect.token"
      SCOPES = %w[openid email profile].freeze
      VALID_ISSUERS = %w[https://slack.com].freeze

      def key = KEY
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def scopes = SCOPES
      def extra_authorization_params = {}

      def identity_from(result, client_id:)
        Login::IdToken.identity(result.id_token, client_id: client_id, valid_issuers: VALID_ISSUERS)
      end
    end
  end
end
