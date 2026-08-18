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

      test "resolves the authenticated Linear viewer" do
        http = expect_http_call(
          status: 200,
          body: { data: { viewer: { id: "LinUser_123", name: "Ada Lovelace", email: "ada@example.com" } } }.to_json
        ) do |request|
          assert_equal :post, request[:method]
          assert_equal Linear::GRAPHQL_ENDPOINT, request[:url]
          assert_equal({ "query" => Linear::VIEWER_QUERY }, JSON.parse(request[:body]))
          assert_equal "Bearer lin_token", request[:headers]["Authorization"]
        end

        identity = Linear.new.identity_from(
          result,
          client_id: "unused",
          http_client: HttpClient.new(http: http)
        )

        assert_equal "LinUser_123", identity[:subject]
        assert_equal "ada@example.com", identity[:email]
        assert_equal "Ada Lovelace", identity[:name]
        http.verify
      end

      test "missing access token raises a parse error" do
        err = assert_raises(Broker::ExchangeError) do
          Linear.new.identity_from(result(access_token: nil), client_id: "unused")
        end
        assert_equal "missing_access_token", err.code
      end

      test "missing viewer raises an exchange error" do
        http = expect_http_call(status: 200, body: { data: { viewer: nil } }.to_json)

        err = assert_raises(Broker::ExchangeError) do
          Linear.new.identity_from(
            result,
            client_id: "unused",
            http_client: HttpClient.new(http: http)
          )
        end

        assert_equal "missing_identity", err.code
        http.verify
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
