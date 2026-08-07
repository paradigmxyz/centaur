require "test_helper"

module Oauth
  module Providers
    class MicrosoftTest < ActiveSupport::TestCase
      CLIENT_ID = "the-client-id".freeze

      def strategy = Microsoft.new

      def result_with(claims:, **overrides)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        id_token = "h.#{payload}.s"
        Broker::AuthorizationCodeClient::Result.new(**{
          access_token: "AT", refresh_token: "RT", expires_in: 3600,
          scope: "openid profile email offline_access User.Read", id_token: id_token, response: {}
        }.merge(overrides))
      end

      def valid_claims(**overrides)
        {
          "aud" => CLIENT_ID,
          "iss" => "https://login.microsoftonline.com/11111111-2222-3333-4444-555555555555/v2.0",
          "sub" => "AAAAAAAAAAAAAAAAAAAAAIkzqFVrSaSaFHy782bbtaQ",
          "email" => "user@example.com",
          "preferred_username" => "user@example.com"
        }.merge(overrides)
      end

      test "happy path extracts subject and email" do
        result = result_with(claims: valid_claims)
        identity = strategy.identity_from(result, client_id: CLIENT_ID)
        assert_equal "AAAAAAAAAAAAAAAAAAAAAIkzqFVrSaSaFHy782bbtaQ", identity[:subject]
        assert_equal "user@example.com", identity[:email]
      end

      test "falls back to preferred_username when email claim is absent" do
        result = result_with(claims: valid_claims.except("email").merge(
          "preferred_username" => "alias@contoso.com"
        ))
        assert_equal "alias@contoso.com", strategy.identity_from(result, client_id: CLIENT_ID)[:email]
      end

      test "accepts aud as an array containing the client id" do
        result = result_with(claims: valid_claims("aud" => [ CLIENT_ID, "other" ]))
        assert_equal "AAAAAAAAAAAAAAAAAAAAAIkzqFVrSaSaFHy782bbtaQ",
                     strategy.identity_from(result, client_id: CLIENT_ID)[:subject]
      end

      test "aud mismatch raises an oauth exchange error" do
        result = result_with(claims: valid_claims("aud" => "someone-else"))
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "oauth", err.stage
        assert_equal "id_token_aud_mismatch", err.code
      end

      test "bad issuer raises" do
        result = result_with(claims: valid_claims("iss" => "https://evil.example/v2.0"))
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
        assert_equal "microsoft", strategy.key
        assert_equal "Microsoft", strategy.display_name
        assert_equal "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                     strategy.authorization_endpoint
        assert_equal "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                     strategy.token_endpoint
        assert_equal %w[openid profile email offline_access], strategy.identity_scopes
        assert_equal %w[graph.microsoft.com], strategy.api_hosts
        assert_equal({ "prompt" => "consent" }, strategy.extra_authorization_params)
        assert strategy.refreshable?
      end
    end
  end
end
