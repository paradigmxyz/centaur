require "base64"
require "json"

module Oauth
  module Providers
    # Google consent-flow strategy. Owns Google's authorization/token endpoints,
    # the extra authorization params that guarantee a refresh token, and how to
    # pull a stable account identity (sub/email) out of a code-exchange result.
    #
    # SECURITY: identity extraction touches the id_token, which carries the
    # account identity but no tokens. As elsewhere under Broker/Oauth, nothing
    # here logs token material.
    class Google
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
      # Decodes the id_token payload without verifying its signature: the token
      # came directly from Google's token endpoint over TLS, which OIDC Core
      # 3.1.3.7.6 accepts as sufficient. Sanity-checks aud == client_id and
      # iss in the known Google issuers. Raises Broker::ExchangeError on any
      # mismatch or a missing/undecodable id_token.
      def identity_from(result, client_id:)
        if result.id_token.blank?
          raise Broker::ExchangeError.new("token response carried no id_token",
                                          stage: "oauth", code: "missing_id_token")
        end

        claims = decode_id_token_claims(result.id_token)

        unless claims["aud"] == client_id
          raise Broker::ExchangeError.new("id_token aud did not match client_id",
                                          stage: "oauth", code: "id_token_aud_mismatch")
        end
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

      private

      # Decodes the JWT payload (second segment), tolerating the unpadded
      # base64url JWTs use. No signature verification -- see identity_from.
      def decode_id_token_claims(id_token)
        seg = id_token.split(".")[1].to_s
        seg += "=" * ((4 - seg.length % 4) % 4)
        JSON.parse(Base64.urlsafe_decode64(seg))
      rescue ArgumentError, JSON::ParserError
        raise Broker::ExchangeError.new("id_token payload did not decode", stage: "parse")
      end
    end
  end
end
