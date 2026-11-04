module Oauth
  module Providers
    # Google consent-flow strategy. Owns Google's authorization/token endpoints,
    # the extra authorization params that guarantee a refresh token, and how to
    # pull a stable account identity (sub/email) out of a code-exchange result.
    class Google
      include IdTokenIdentity

      KEY = "google"
      AUTHORIZATION_ENDPOINT = "https://accounts.google.com/o/oauth2/v2/auth"
      TOKEN_ENDPOINT = "https://oauth2.googleapis.com/token"
      # Always requested in addition to the app's scopes, so the token response
      # carries an id_token identifying the Google account.
      IDENTITY_SCOPES = %w[openid https://www.googleapis.com/auth/userinfo.email].freeze
      # Hosts a minted access token may be sent to, as request-rule host patterns.
      # Every Google API is served under *.googleapis.com; accounts.google.com is
      # auth-only and intentionally excluded. Drives the rules on the static secret
      # auto-created alongside a minted credential.
      API_HOSTS = %w[*.googleapis.com].freeze
      # The issuers Google stamps into the id_token; both forms are accepted per
      # Google's OIDC discovery document.
      VALID_ISSUERS = %w[https://accounts.google.com accounts.google.com].freeze

      def key = KEY
      def display_name = "Google"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def refreshable? = true

      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(scopes) = Array(scopes)

      # Provider-specific query params for the authorization redirect. Both are
      # required to guarantee a refresh token, including on re-consent:
      # access_type=offline asks for one at all, prompt=consent forces a fresh
      # one even when the user has consented before.
      def extra_authorization_params = { "access_type" => "offline", "prompt" => "consent" }

      # Extracts { subject:, email: } from a successful code-exchange result.
      # Sanity-checks aud == client_id and iss in the known Google issuers. Raises
      # Broker::ExchangeError on any mismatch or a missing/undecodable id_token.
      def identity_from(result, client_id:, http_client: nil)
        claims = id_token_claims(result)
        require_audience!(claims, client_id)
        unless VALID_ISSUERS.include?(claims["iss"])
          raise Broker::ExchangeError.new("id_token iss was not a Google issuer",
                                          stage: "oauth", code: "id_token_iss_invalid")
        end

        subject = claims["sub"]
        if subject.blank?
          raise Broker::ExchangeError.new("id_token carried no sub",
                                          stage: "oauth", code: "id_token_missing_sub")
        end

        { subject: subject, email: claims["email"] }
      end
    end
  end
end
