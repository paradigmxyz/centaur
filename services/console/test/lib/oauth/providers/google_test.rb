require "test_helper"

module Oauth
  module Providers
    class GoogleTest < ActiveSupport::TestCase
      CLIENT_ID = "the-client-id".freeze

      def strategy = Google.new

      # Builds a result whose id_token encodes +claims+ as a JWT-shaped string
      # (the strategy decodes the payload segment without verifying the signature).
      def result_with(claims:, **overrides)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        id_token = "h.#{payload}.s"
        Broker::AuthorizationCodeClient::Result.new(**{
          access_token: "AT", refresh_token: "RT", expires_in: 3600,
          scope: "openid email", id_token: id_token
        }.merge(overrides))
      end

      def valid_claims(**overrides)
        { "aud" => CLIENT_ID, "iss" => "https://accounts.google.com",
          "sub" => "1234567890", "email" => "user@example.com" }.merge(overrides)
      end

      test "happy path extracts subject and email" do
        result = result_with(claims: valid_claims)
        identity = strategy.identity_from(result, client_id: CLIENT_ID)
        assert_equal "1234567890", identity[:subject]
        assert_equal "user@example.com", identity[:email]
      end

      test "accepts the bare accounts.google.com issuer" do
        result = result_with(claims: valid_claims("iss" => "accounts.google.com"))
        assert_equal "1234567890", strategy.identity_from(result, client_id: CLIENT_ID)[:subject]
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
          scope: nil, id_token: "h.!!!not-base64!!!.s"
        )
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "parse", err.stage
      end

      test "exposes provider constants" do
        assert_equal "google", strategy.key
        assert_equal "https://accounts.google.com/o/oauth2/v2/auth", strategy.authorization_endpoint
        assert_equal "https://oauth2.googleapis.com/token", strategy.token_endpoint
        assert_equal({ "access_type" => "offline", "prompt" => "consent" }, strategy.extra_authorization_params)
      end
    end
  end
end
