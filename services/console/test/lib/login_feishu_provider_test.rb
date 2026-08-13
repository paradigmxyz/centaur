require "test_helper"

module Login
  module Providers
    class FeishuTest < ActiveSupport::TestCase
      CLIENT_ID = "cli_feishu_test".freeze
      CLIENT_SECRET = "feishu-secret".freeze
      REDIRECT_URI = "https://console.example.test/auth/feishu/callback".freeze

      test "uses only China Feishu OAuth endpoints and PKCE" do
        provider = Feishu.new

        assert_equal "https://accounts.feishu.cn/open-apis/authen/v1/authorize", provider.authorization_endpoint
        assert_equal "https://accounts.feishu.cn/oauth/v3/token", provider.token_endpoint
        assert_equal "https://open.feishu.cn/open-apis/authen/v1/user_info", provider.user_info_endpoint
        assert provider.pkce?
        assert_includes provider.scopes, "contact:user.employee:readonly"
      end

      test "encodes tenant and union id as an unambiguous stable subject" do
        first = Feishu.subject(tenant_key: "tenant-a", union_id: "on-shared")

        assert_equal first, Feishu.subject(tenant_key: "tenant-a", union_id: "on-shared")
        assert_not_equal first, Feishu.subject(tenant_key: "tenant-b", union_id: "on-shared")
        assert_equal [ "tenant-a", "on-shared" ], JSON.parse(first)
      end

      test "exchanges through v3 and returns a tenant-scoped identity" do
        http = successful_http

        identity = Feishu.new.exchange_identity(
          code: "one-time-code",
          client_id: CLIENT_ID,
          client_secret: CLIENT_SECRET,
          redirect_uri: REDIRECT_URI,
          code_verifier: "v" * 64,
          allowed_tenant_keys: [ "tenant-a" ],
          http: HttpClient.new(http: http)
        )

        assert_equal Feishu.subject(tenant_key: "tenant-a", union_id: "on-user"), identity[:subject]
        assert_equal "tenant-a", identity[:tenant_key]
        assert_equal "ou-user", identity[:open_id]
        assert_equal "user@corp.example", identity[:email]
        assert_equal true, identity[:email_verified]
        assert_equal "Test User", identity[:name]
        http.verify
      end

      test "rejects a user outside the configured tenant allowlist" do
        http = successful_http

        error = assert_raises(Broker::ExchangeError) do
          exchange(http: HttpClient.new(http:), allowed_tenant_keys: [ "tenant-b" ])
        end

        assert_equal "tenant_not_allowed", error.reason
        http.verify
      end

      test "fails closed without union id open id or enterprise email" do
        {
          "union_id" => nil,
          "open_id" => " ",
          "enterprise_email" => nil
        }.each do |field, value|
          http = successful_http(user_overrides: { field => value })

          error = assert_raises(Broker::ExchangeError) { exchange(http: HttpClient.new(http:)) }

          assert_equal "invalid_user_info", error.reason
          http.verify
        end
      end

      test "sanitizes token and user info provider failures" do
        token_http = Minitest::Mock.new
        token_http.expect(
          :call,
          HttpClient::Response.new(status: 400, body: { code: 20025, error: "invalid_grant" }.to_json, headers: {})
        ) do |method:, url:, **|
          method == :post && url == Feishu::TOKEN_ENDPOINT
        end
        token_error = assert_raises(Broker::ExchangeError) do
          exchange(http: HttpClient.new(http: token_http))
        end
        assert_equal "invalid_grant", token_error.reason
        refute_includes token_error.message, "one-time-code"
        refute_includes token_error.message, CLIENT_SECRET
        token_http.verify

        user_http = successful_http(user_status: 403, user_body: { code: 999_916_72, msg: "forbidden" })
        user_error = assert_raises(Broker::ExchangeError) { exchange(http: HttpClient.new(http: user_http)) }
        assert_equal "userinfo_http", user_error.reason
        refute_includes user_error.message, "AT-sensitive"
        user_http.verify
      end

      private

      def exchange(http:, allowed_tenant_keys: [ "tenant-a" ])
        Feishu.new.exchange_identity(
          code: "one-time-code",
          client_id: CLIENT_ID,
          client_secret: CLIENT_SECRET,
          redirect_uri: REDIRECT_URI,
          code_verifier: "v" * 64,
          allowed_tenant_keys:,
          http: http
        )
      end

      def successful_http(user_overrides: {}, user_status: 200, user_body: nil)
        http = Minitest::Mock.new
        http.expect(
          :call,
          HttpClient::Response.new(
            status: 200,
            body: { code: 0, access_token: "AT-sensitive", expires_in: 7200, token_type: "Bearer" }.to_json,
            headers: {}
          )
        ) do |method:, url:, body:, headers:, **|
          payload = JSON.parse(body)
          method == :post && url == Feishu::TOKEN_ENDPOINT &&
            headers["Content-Type"] == "application/json" &&
            payload == {
              "grant_type" => "authorization_code",
              "client_id" => CLIENT_ID,
              "client_secret" => CLIENT_SECRET,
              "code" => "one-time-code",
              "redirect_uri" => REDIRECT_URI,
              "code_verifier" => "v" * 64
            }
        end
        body = user_body || {
          "code" => 0,
          "msg" => "success",
          "data" => {
            "tenant_key" => "tenant-a",
            "open_id" => "ou-user",
            "union_id" => "on-user",
            "enterprise_email" => "user@corp.example",
            "name" => "Test User"
          }.merge(user_overrides)
        }
        http.expect(
          :call,
          HttpClient::Response.new(status: user_status, body: body.to_json, headers: {})
        ) do |method:, url:, body:, headers:, **|
          method == :get && url == Feishu::USER_INFO_ENDPOINT && body.nil? &&
            headers["Authorization"] == "Bearer AT-sensitive"
        end
        http
      end
    end
  end
end
