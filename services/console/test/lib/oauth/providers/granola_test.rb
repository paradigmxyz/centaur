require "test_helper"

module Oauth
  module Providers
    class GranolaTest < ActiveSupport::TestCase
      CLIENT_ID = "the-client-id".freeze

      def strategy = Granola.new

      # Builds a result whose id_token encodes +claims+ as a JWT-shaped string
      # (the strategy decodes the payload segment without verifying the signature).
      def result_with(claims:, **overrides)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        id_token = "h.#{payload}.s"
        Broker::AuthorizationCodeClient::Result.new(**{
          access_token: "AT", refresh_token: "RT", expires_in: 3600,
          scope: "openid email profile offline_access mcp", id_token: id_token, response: {}
        }.merge(overrides))
      end

      def valid_claims(**overrides)
        { "aud" => CLIENT_ID, "iss" => "https://mcp-auth.granola.ai",
          "sub" => "granola-user-123", "email" => "user@example.com" }.merge(overrides)
      end

      test "happy path extracts subject and email" do
        result = result_with(claims: valid_claims)
        identity = strategy.identity_from(result, client_id: CLIENT_ID)
        assert_equal "granola-user-123", identity[:subject]
        assert_equal "user@example.com", identity[:email]
      end

      test "aud mismatch raises an oauth exchange error" do
        result = result_with(claims: valid_claims("aud" => "someone-else"))
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "oauth", err.stage
        assert_equal "id_token_aud_mismatch", err.code
      end

      test "bad issuer raises" do
        result = result_with(claims: valid_claims("iss" => "https://evil.example"))
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "id_token_iss_invalid", err.code
      end

      test "missing id_token raises" do
        result = result_with(claims: valid_claims, id_token: nil)
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "missing_id_token", err.code
      end

      test "missing sub raises" do
        result = result_with(claims: valid_claims.except("sub"))
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "id_token_missing_sub", err.code
      end

      test "undecodable payload raises a parse error" do
        result = Broker::AuthorizationCodeClient::Result.new(
          access_token: "AT", refresh_token: "RT", expires_in: 3600,
          scope: nil, id_token: "h.!!!not-base64!!!.s", response: {}
        )
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "parse", err.stage
      end

      test "exposes provider constants" do
        assert_equal "granola", strategy.key
        assert_equal "Granola", strategy.display_name
        assert_equal "https://mcp-auth.granola.ai/oauth2/authorize", strategy.authorization_endpoint
        assert_equal "https://mcp-auth.granola.ai/oauth2/token", strategy.token_endpoint
        assert_equal %w[openid email profile offline_access], strategy.identity_scopes
        assert_equal %w[mcp.granola.ai], strategy.api_hosts
        assert_equal "scope", strategy.authorization_scope_param
        assert_equal " ", strategy.scope_separator
        assert_equal({}, strategy.extra_authorization_params)
        assert_predicate strategy, :refreshable?
      end

      test "parses and refreshes scopes like an OIDC provider" do
        assert_equal %w[openid email mcp], strategy.parse_granted_scopes("openid email mcp")
        scopes = %w[mcp openid offline_access]
        assert_equal scopes, strategy.refresh_scopes(scopes)
      end
    end
  end
end
