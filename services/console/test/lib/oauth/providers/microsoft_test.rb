require "test_helper"

module Oauth
  module Providers
    class MicrosoftTest < ActiveSupport::TestCase
      CLIENT_ID = "the-client-id".freeze
      ISSUER = "https://login.microsoftonline.com/72f988bf-86f1-41af-91ab-2d7cd011db47/v2.0".freeze

      def strategy = Microsoft.new

      # Builds a result whose id_token encodes +claims+ as a JWT-shaped string
      # (the strategy decodes the payload segment without verifying the signature).
      def result_with(claims:, **overrides)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        Broker::AuthorizationCodeClient::Result.new(**{
          access_token: "AT", refresh_token: "RT", expires_in: 3600,
          scope: "Mail.Read", id_token: "h.#{payload}.s", response: {}
        }.merge(overrides))
      end

      def valid_claims(**overrides)
        { "aud" => CLIENT_ID, "iss" => ISSUER, "oid" => "0c6b8a4e-1a2b-4c3d-9e8f-123456789abc",
          "sub" => "pairwise-sub", "email" => "user@example.com",
          "preferred_username" => "user@example.onmicrosoft.com", "name" => "Example User" }.merge(overrides)
      end

      test "happy path extracts the object id, email and name" do
        identity = strategy.identity_from(result_with(claims: valid_claims), client_id: CLIENT_ID)
        assert_equal "0c6b8a4e-1a2b-4c3d-9e8f-123456789abc", identity[:subject]
        assert_equal "user@example.com", identity[:email]
        assert_equal "Example User", identity[:name]
      end

      test "falls back to preferred_username when the email claim is absent" do
        result = result_with(claims: valid_claims.except("email"))
        assert_equal "user@example.onmicrosoft.com", strategy.identity_from(result, client_id: CLIENT_ID)[:email]
      end

      test "aud mismatch raises an oauth exchange error" do
        result = result_with(claims: valid_claims("aud" => "someone-else"))
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "oauth", err.stage
        assert_equal "id_token_aud_mismatch", err.code
      end

      test "rejects issuers outside login.microsoftonline.com v2.0" do
        [ "https://sts.windows.net/72f988bf-86f1-41af-91ab-2d7cd011db47/",
          "https://login.microsoftonline.com/common/v2.0",
          "https://evil.example/72f988bf-86f1-41af-91ab-2d7cd011db47/v2.0" ].each do |iss|
          result = result_with(claims: valid_claims("iss" => iss))
          err = assert_raises(Broker::ExchangeError, iss) { strategy.identity_from(result, client_id: CLIENT_ID) }
          assert_equal "id_token_iss_invalid", err.code
        end
      end

      test "missing id_token raises" do
        result = result_with(claims: valid_claims, id_token: nil)
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "missing_id_token", err.code
      end

      test "missing oid raises" do
        result = result_with(claims: valid_claims.except("oid"))
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "id_token_missing_oid", err.code
      end

      test "undecodable payload raises a parse error" do
        result = result_with(claims: {}, id_token: "h.!!!not-base64!!!.s")
        err = assert_raises(Broker::ExchangeError) { strategy.identity_from(result, client_id: CLIENT_ID) }
        assert_equal "parse", err.stage
      end

      test "exposes provider constants" do
        assert_equal "microsoft", strategy.key
        assert_equal "https://login.microsoftonline.com/common/oauth2/v2.0/authorize", strategy.authorization_endpoint
        assert_equal "https://login.microsoftonline.com/common/oauth2/v2.0/token", strategy.token_endpoint
        assert_equal %w[openid email profile offline_access], strategy.identity_scopes
        assert_equal [ "graph.microsoft.com" ], strategy.api_hosts
        assert_equal({ "prompt" => "select_account" }, strategy.extra_authorization_params)
        assert_equal %w[Mail.Read Calendars.Read], strategy.parse_granted_scopes("Mail.Read Calendars.Read")
        assert_equal %w[Mail.Read], strategy.refresh_scopes(%w[Mail.Read])
      end
    end
  end
end
