require "test_helper"

module Oauth
  module Providers
    class SlackTest < ActiveSupport::TestCase
      CLIENT_ID = "the-client-id".freeze

      def strategy = Slack.new

      def result_with(claims:, scope: "openid,email,profile", **overrides)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        id_token = "h.#{payload}.s"
        Broker::AuthorizationCodeClient::Result.new(**{
          access_token: "AT", refresh_token: "RT", expires_in: 43_200,
          scope: scope, id_token: id_token, response: {}
        }.merge(overrides))
      end

      def valid_claims(**overrides)
        { "aud" => CLIENT_ID, "iss" => "https://slack.com",
          "sub" => "U0R7MFMJM", "email" => "user@example.com" }.merge(overrides)
      end

      test "happy path extracts subject and email" do
        identity = strategy.identity_from(result_with(claims: valid_claims), client_id: CLIENT_ID)
        assert_equal "U0R7MFMJM", identity[:subject]
        assert_equal "user@example.com", identity[:email]
      end

      test "uses Slack authed_user id when no id_token is returned for API scopes" do
        result = result_with(
          claims: valid_claims,
          id_token: nil,
          response: { "authed_user" => { "id" => "U12345" } }
        )

        identity = strategy.identity_from(result, client_id: CLIENT_ID)
        assert_equal "U12345", identity[:subject]
        assert_nil identity[:email]
        assert_nil identity[:name]
      end

      test "uses Slack response name when present" do
        result = result_with(
          claims: valid_claims,
          id_token: nil,
          response: { "authed_user" => { "id" => "U12345", "name" => "ada" } }
        )

        identity = strategy.identity_from(result, client_id: CLIENT_ID)
        assert_equal "ada", identity[:name]
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

      test "parses comma or space separated granted scopes" do
        assert_equal %w[openid email profile], strategy.parse_granted_scopes("openid,email profile")
      end

      test "exposes provider constants" do
        assert_equal "slack", strategy.key
        assert_equal "https://slack.com/oauth/v2/authorize", strategy.authorization_endpoint
        assert_equal "https://slack.com/api/oauth.v2.access", strategy.token_endpoint
        assert_equal [], strategy.identity_scopes
        assert_equal "user_scope", strategy.authorization_scope_param
        assert_equal ",", strategy.scope_separator
        assert_equal({}, strategy.extra_authorization_params)
      end
    end
  end
end
