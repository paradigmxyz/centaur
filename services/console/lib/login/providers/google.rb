module Login
  module Providers
    # Google "Sign in with Google" strategy for console login. Unlike the broker's
    # Oauth::Providers::Google it requests no offline access (login needs only a
    # one-shot identity, never a refresh token), so it has no extra authorization
    # params and a minimal openid/email/profile scope set.
    class Google
      KEY = "google"
      AUTHORIZATION_ENDPOINT = "https://accounts.google.com/o/oauth2/v2/auth"
      TOKEN_ENDPOINT = "https://oauth2.googleapis.com/token"
      SCOPES = %w[openid email profile].freeze
      VALID_ISSUERS = %w[https://accounts.google.com accounts.google.com].freeze

      def key = KEY
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def scopes = SCOPES
      def extra_authorization_params = {}
      def pkce? = true
      def token_exchange_client_secret(secret) = secret

      def identity_from(result, client_id:)
        Login::IdToken.identity(result.id_token, client_id: client_id, valid_issuers: VALID_ISSUERS)
      end
    end
  end
end
