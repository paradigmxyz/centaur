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

      test "builds a deterministic pending identity without calling GitHub" do
        identity = Github.new.identity_from(result, client_id: "unused")

        assert_match(/\Apending-[a-f0-9]{32}\z/, identity[:subject])
        assert_nil identity[:email]
        assert_equal "Pending GitHub account", identity[:name]
        assert_equal identity, Github.new.identity_from(result, client_id: "unused")
      end

      test "missing access token raises a parse error" do
        err = assert_raises(Broker::ExchangeError) do
          Github.new.identity_from(result(access_token: nil), client_id: "unused")
        end
        assert_equal "missing_access_token", err.code
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
        assert_equal "scope", strategy.authorization_scope_param
        assert_equal " ", strategy.scope_separator
        refute strategy.refreshable?
      end
    end
  end
end
