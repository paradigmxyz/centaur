require "test_helper"

module Oauth
  module Providers
    class LinearTest < ActiveSupport::TestCase
      def result(access_token: "lin_token", scope: "read write")
        Broker::AuthorizationCodeClient::Result.new(
          access_token: access_token, refresh_token: "lin-refresh", expires_in: 86_399,
          scope: scope, id_token: nil, response: {}
        )
      end

      test "builds a deterministic pending identity without calling Linear" do
        identity = Linear.new.identity_from(result, client_id: "unused")

        assert_match(/\Apending-[a-f0-9]{32}\z/, identity[:subject])
        assert_nil identity[:email]
        assert_equal "Pending Linear account", identity[:name]
        assert_equal identity, Linear.new.identity_from(result, client_id: "unused")
      end

      test "missing access token raises a parse error" do
        err = assert_raises(Broker::ExchangeError) do
          Linear.new.identity_from(result(access_token: nil), client_id: "unused")
        end
        assert_equal "missing_access_token", err.code
      end

      test "parses space separated granted scopes" do
        assert_equal %w[read write issues:create], Linear.new.parse_granted_scopes("read write issues:create")
      end

      test "parses array-form granted scopes from older apps" do
        assert_equal %w[read write], Linear.new.parse_granted_scopes([ "read", "write", "" ])
      end

      test "exposes provider constants" do
        strategy = Linear.new
        assert_equal "linear", strategy.key
        assert_equal "Linear", strategy.display_name
        assert_equal "https://linear.app/oauth/authorize", strategy.authorization_endpoint
        assert_equal "https://api.linear.app/oauth/token", strategy.token_endpoint
        assert_equal [], strategy.identity_scopes
        assert_equal [ "api.linear.app" ], strategy.api_hosts
        assert_equal "scope", strategy.authorization_scope_param
        assert_equal ",", strategy.scope_separator
        assert_equal({}, strategy.extra_authorization_params)
        assert strategy.refreshable?
        assert_equal %w[read write], strategy.refresh_scopes(%w[read write])
      end
    end
  end
end
