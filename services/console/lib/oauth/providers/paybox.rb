require "base64"
require "json"

module Oauth
  module Providers
    # PayBox OAuth 2.1 consent-flow strategy for its MCP endpoint. PayBox uses
    # dynamically registered public clients, PKCE S256, and rotating refresh
    # tokens. The access token is a JWT audience-bound to the MCP resource.
    class Paybox
      KEY = "paybox"
      ISSUER = "https://api.paybox.sh"
      AUTHORIZATION_ENDPOINT = "#{ISSUER}/oauth/authorize"
      TOKEN_ENDPOINT = "#{ISSUER}/oauth/token"
      REGISTRATION_ENDPOINT = "#{ISSUER}/oauth/register"
      MCP_RESOURCE = "#{ISSUER}/mcp"
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.paybox.sh].freeze

      def key = KEY
      def display_name = "PayBox"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = { "resource" => MCP_RESOURCE }
      def refreshable? = true
      def public_client? = true

      def parse_granted_scopes(scope) = scope.to_s.split

      # PayBox binds scopes to the rotating refresh-token family; its documented
      # refresh request does not send a scope parameter.
      def refresh_scopes(_scopes) = []

      # PayBox returns no OIDC id_token. Decode the access-token claims to obtain
      # the stable subject. The token came directly from PayBox's token endpoint
      # over TLS; issuer, audience, and client id are still checked before use.
      def identity_from(result, client_id:, http_client: nil)
        claims = decode_access_token_claims(result.access_token)
        unless claims["iss"] == ISSUER
          raise Broker::ExchangeError.new("access token iss was not PayBox",
                                          stage: "oauth", code: "access_token_iss_invalid")
        end

        audiences = Array(claims["aud"])
        unless audiences.include?(MCP_RESOURCE)
          raise Broker::ExchangeError.new("access token aud did not include the PayBox MCP resource",
                                          stage: "oauth", code: "access_token_aud_mismatch")
        end

        if claims["cid"].present? && claims["cid"] != client_id
          raise Broker::ExchangeError.new("access token cid did not match client_id",
                                          stage: "oauth", code: "access_token_client_mismatch")
        end

        subject = claims["sub"]
        if subject.blank?
          raise Broker::ExchangeError.new("access token carried no sub",
                                          stage: "oauth", code: "access_token_missing_sub")
        end

        { subject: subject, email: claims["email"].presence }
      end

      private

      def decode_access_token_claims(access_token)
        segment = access_token.to_s.split(".")[1].to_s
        segment += "=" * ((4 - segment.length % 4) % 4)
        JSON.parse(Base64.urlsafe_decode64(segment))
      rescue ArgumentError, JSON::ParserError
        raise Broker::ExchangeError.new("access token payload did not decode", stage: "parse")
      end
    end
  end
end
