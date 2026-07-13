require "test_helper"

module Oauth
  module Providers
    class AttioTest < ActiveSupport::TestCase
      def result(access_token: "attio-token", scope: nil)
        Broker::AuthorizationCodeClient::Result.new(
          access_token: access_token, refresh_token: nil, expires_in: nil,
          scope: scope, id_token: nil, response: {}
        )
      end

      test "builds a deterministic pending identity without calling Attio" do
        identity = Attio.new.identity_from(result, client_id: "unused")

        assert_match(/\Apending-[a-f0-9]{32}\z/, identity[:subject])
        assert_nil identity[:email]
        assert_equal "Pending Attio workspace", identity[:name]
        assert_equal identity, Attio.new.identity_from(result, client_id: "unused")
      end

      test "missing access token raises a parse error" do
        err = assert_raises(Broker::ExchangeError) do
          Attio.new.identity_from(result(access_token: nil), client_id: "unused")
        end
        assert_equal "missing_access_token", err.code
      end

      test "nil scope parses to an empty list" do
        assert_equal [], Attio.new.parse_granted_scopes(nil)
      end

      test "parses comma or space separated granted scopes when present" do
        assert_equal %w[record_permission:read object_configuration:read],
                     Attio.new.parse_granted_scopes("record_permission:read,object_configuration:read")
      end

      test "exposes provider constants" do
        strategy = Attio.new
        assert_equal "attio", strategy.key
        assert_equal "Attio", strategy.display_name
        assert_equal "https://app.attio.com/authorize", strategy.authorization_endpoint
        assert_equal "https://app.attio.com/oauth/token", strategy.token_endpoint
        assert_equal [], strategy.identity_scopes
        assert_equal %w[api.attio.com], strategy.api_hosts
        assert_equal "scope", strategy.authorization_scope_param
        assert_equal " ", strategy.scope_separator
        assert_equal({}, strategy.extra_authorization_params)
        refute strategy.refreshable?
      end
    end
  end
end
