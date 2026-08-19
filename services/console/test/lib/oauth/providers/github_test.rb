require "test_helper"

module Oauth
  module Providers
    class GithubTest < ActiveSupport::TestCase
      def result(access_token: "gho_token", scope: "repo,read:user")
        Broker::AuthorizationCodeClient::Result.new(
          access_token: access_token, refresh_token: nil, expires_in: nil,
          scope: scope, id_token: nil, response: {}
        )
      end

      test "resolves the authenticated GitHub user" do
        http = expect_http_call(
          status: 200,
          body: { id: 99_123, login: "octocat", name: "Octo Cat", email: "octo@example.com" }.to_json
        ) do |request|
          assert_equal :get, request[:method]
          assert_equal Github::USER_ENDPOINT, request[:url]
          assert_equal "Bearer gho_token", request[:headers]["Authorization"]
          assert_equal "application/vnd.github+json", request[:headers]["Accept"]
        end

        identity = Github.new.identity_from(
          result,
          client_id: "unused",
          http_client: HttpClient.new(http: http)
        )

        assert_equal "99123", identity[:subject]
        assert_equal "octo@example.com", identity[:email]
        assert_equal "Octo Cat", identity[:name]
        assert_equal({ "github_login" => "octocat" }, identity[:labels])
        http.verify
      end

      test "falls back to the GitHub login for the display name" do
        http = expect_http_call(
          status: 200,
          body: { id: 99_123, login: "octocat", name: nil, email: nil }.to_json
        )

        identity = Github.new.identity_from(
          result,
          client_id: "unused",
          http_client: HttpClient.new(http: http)
        )

        assert_equal "octocat", identity[:name]
        assert_nil identity[:email]
        http.verify
      end

      test "missing access token raises a parse error" do
        err = assert_raises(Broker::ExchangeError) do
          Github.new.identity_from(result(access_token: nil), client_id: "unused")
        end
        assert_equal "missing_access_token", err.code
      end

      test "identity endpoint failure raises an exchange error" do
        http = expect_http_call(status: 503, body: "temporarily unavailable")

        err = assert_raises(Broker::ExchangeError) do
          Github.new.identity_from(
            result,
            client_id: "unused",
            http_client: HttpClient.new(http: http)
          )
        end

        assert_equal "identity_lookup_failed", err.code
        assert_equal 503, err.status
        http.verify
      end

      test "identity endpoint network failure raises a network exchange error" do
        transport = ->(**) { raise Errno::ECONNREFUSED }

        err = assert_raises(Broker::ExchangeError) do
          Github.new.identity_from(
            result,
            client_id: "unused",
            http_client: HttpClient.new(http: transport)
          )
        end

        assert_equal "identity_lookup_failed", err.code
        assert_equal "network", err.stage
      end

      test "parses comma or space separated granted scopes" do
        assert_equal %w[repo read:user gist], Github.new.parse_granted_scopes("repo,read:user gist")
      end

      test "exposes provider constants" do
        strategy = Github.new
        assert_equal "github", strategy.key
        assert_equal "GitHub", strategy.display_name
        assert_equal "https://github.com/login/oauth/authorize", strategy.authorization_endpoint
        assert_equal "https://github.com/login/oauth/access_token", strategy.token_endpoint
        assert_equal [], strategy.identity_scopes
        assert_equal "github_token", strategy.wrapping_secret_kind
        assert_equal "scope", strategy.authorization_scope_param
        assert_equal " ", strategy.scope_separator
        refute strategy.refreshable?
      end
    end
  end
end
