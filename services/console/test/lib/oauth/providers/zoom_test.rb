require "test_helper"

module Oauth
  module Providers
    class ZoomTest < ActiveSupport::TestCase
      def result
        Broker::AuthorizationCodeClient::Result.new(
          access_token: "zoom-token", refresh_token: "zoom-refresh", expires_in: 3600,
          scope: "meeting:write recording:read", id_token: nil, response: {}
        )
      end

      test "resolves the authenticated Zoom user" do
        http = expect_http_call(
          status: 200,
          body: { id: "zoom-user-id", email: "scheduler@example.com", display_name: "Scheduler" }.to_json
        ) do |request|
          assert_equal :get, request[:method]
          assert_equal Zoom::SELF_ENDPOINT, request[:url]
          assert_equal "Bearer zoom-token", request[:headers]["Authorization"]
        end

        identity = Zoom.new.identity_from(result, client_id: "unused", http_client: HttpClient.new(http: http))
        assert_equal "zoom-user-id", identity[:subject]
        assert_equal "scheduler@example.com", identity[:email]
        assert_equal "Scheduler", identity[:name]
        http.verify
      end

      test "exposes Zoom OAuth behavior" do
        strategy = Zoom.new
        assert_equal "https://zoom.us/oauth/authorize", strategy.authorization_endpoint
        assert_equal "https://zoom.us/oauth/token", strategy.token_endpoint
        assert_equal [ "api.zoom.us" ], strategy.api_hosts
        assert_equal :client_secret_basic, strategy.token_endpoint_auth_method
        assert_equal "zoom-meeting-scheduler", strategy.credential_foreign_id(
          app_slug: "zoom-meeting-scheduler", identity: { subject: "ignored" }
        )
        assert_equal %w[meeting:write recording:read], strategy.parse_granted_scopes("meeting:write recording:read")
        assert_equal [], strategy.refresh_scopes(%w[meeting:write])
      end
    end
  end
end
