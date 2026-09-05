require "test_helper"

module Oauth
  module Providers
    class PayboxTest < ActiveSupport::TestCase
      CLIENT_ID = "pbx-oauth-client".freeze

      def strategy = Paybox.new

      def result_with(claims)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        Broker::AuthorizationCodeClient::Result.new(
          access_token: "h.#{payload}.s",
          refresh_token: "RT",
          expires_in: 3600,
          scope: "mcp offline_access",
          id_token: nil,
          response: {}
        )
      end

      def valid_claims(**overrides)
        {
          "iss" => Paybox::ISSUER,
          "aud" => Paybox::MCP_RESOURCE,
          "sub" => "paybox-user-123",
          "cid" => CLIENT_ID,
          "email" => "user@example.com"
        }.merge(overrides)
      end

      test "extracts identity from an audience-bound access token" do
        identity = strategy.identity_from(result_with(valid_claims), client_id: CLIENT_ID)
        assert_equal "paybox-user-123", identity[:subject]
        assert_equal "user@example.com", identity[:email]
      end

      test "rejects a token for another MCP resource" do
        error = assert_raises(Broker::ExchangeError) do
          strategy.identity_from(result_with(valid_claims("aud" => "https://evil.example/mcp")), client_id: CLIENT_ID)
        end
        assert_equal "access_token_aud_mismatch", error.code
      end

      test "rejects a token for another client" do
        error = assert_raises(Broker::ExchangeError) do
          strategy.identity_from(result_with(valid_claims("cid" => "other-client")), client_id: CLIENT_ID)
        end
        assert_equal "access_token_client_mismatch", error.code
      end

      test "rejects an undecodable access token" do
        result = Broker::AuthorizationCodeClient::Result.new(
          access_token: "not-a-jwt", refresh_token: "RT", expires_in: 3600,
          scope: "mcp offline_access", id_token: nil, response: {}
        )
        error = assert_raises(Broker::ExchangeError) do
          strategy.identity_from(result, client_id: CLIENT_ID)
        end
        assert_equal "parse", error.stage
      end

      test "exposes PayBox public-client OAuth configuration" do
        assert_equal "https://api.paybox.sh/oauth/authorize", strategy.authorization_endpoint
        assert_equal "https://api.paybox.sh/oauth/token", strategy.token_endpoint
        assert_equal({ "resource" => "https://api.paybox.sh/mcp" }, strategy.extra_authorization_params)
        assert_equal %w[api.paybox.sh], strategy.api_hosts
        assert_predicate strategy, :public_client?
        assert_predicate strategy, :refreshable?
        assert_equal [], strategy.refresh_scopes(%w[mcp offline_access])
      end
    end
  end
end
