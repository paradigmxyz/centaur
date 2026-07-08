require "base64"
require "json"

module Oauth
  module Providers
    # Granola consent-flow strategy. This targets Granola's OAuth server for its
    # MCP endpoint (https://mcp.granola.ai/mcp), which Granola documents as
    # browser OAuth for MCP rather than as a classic third-party app dashboard.
    # Operators obtain the OAuth client once via RFC 7591 dynamic client
    # registration at https://mcp-auth.granola.ai/oauth2/register, and Granola may
    # change this MCP-backed availability over time.
    #
    # SECURITY: identity extraction touches the id_token, which carries the
    # account identity but no tokens. As elsewhere under Broker/Oauth, nothing
    # here logs token material.
    class Granola
      KEY = "granola"
      AUTHORIZATION_ENDPOINT = "https://mcp-auth.granola.ai/oauth2/authorize"
      TOKEN_ENDPOINT = "https://mcp-auth.granola.ai/oauth2/token"
      # Always requested in addition to the app's API scopes, so the token
      # response carries an id_token identifying the Granola account. Granola's
      # authorization server requires offline_access to issue a refresh token;
      # request it here because the consent flow requires refreshable credentials.
      IDENTITY_SCOPES = %w[openid email profile offline_access].freeze
      # The access token is for Granola's MCP protected resource.
      API_HOSTS = %w[mcp.granola.ai].freeze
      VALID_ISSUERS = %w[https://mcp-auth.granola.ai].freeze

      def key = KEY
      def display_name = "Granola"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = {}
      def refreshable? = true

      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(scopes) = Array(scopes)

      # Extracts { subject:, email: } from a successful code-exchange result.
      # Decodes the id_token payload without verifying its signature: the token
      # came directly from Granola's token endpoint over TLS, which OIDC Core
      # 3.1.3.7.6 accepts as sufficient. Sanity-checks aud == client_id and
      # iss in the known Granola issuer. Raises Broker::ExchangeError on any
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
          raise Broker::ExchangeError.new("id_token iss was not a Granola issuer",
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
