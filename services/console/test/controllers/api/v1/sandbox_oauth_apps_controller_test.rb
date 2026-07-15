require "test_helper"

module Api
  module V1
    class SandboxOauthAppsControllerTest < ActionDispatch::IntegrationTest
      setup do
        @proxy = proxies(:acme_proxy)
      end

      test "returns enabled OAuth app start URLs for a valid sandbox token" do
        with_env(
          "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
          "CENTAUR_CONSOLE_PUBLIC_URL" => "https://console.example.test"
        ) do
          get "/api/v1/sandbox/oauth_apps", headers: auth_headers(token_for(@proxy))
        end
        assert_response :ok

        data = json_body.fetch("data")
        slugs = data.map { |app| app.fetch("slug") }
        assert_equal slugs.sort, slugs
        assert_includes slugs, "google"
        refute_includes slugs, "google-disabled"

        google = data.find { |app| app.fetch("slug") == "google" }
        assert_equal oauth_apps(:acme_google).oid, google.fetch("id")
        assert_equal "google", google.fetch("provider")
        assert_equal({ "team" => "comms" }, google.fetch("labels"))
        assert_equal oauth_apps(:acme_google).allowed_scopes, google.fetch("allowed_scopes")
        assert_equal "https://console.example.test/oauth/google/start", google.fetch("start_url")
        refute google.key?("client_id")
      end

      test "does not reject a valid token when proxy claims are stale" do
        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          token = token_for(@proxy)
          @proxy.update!(principal: principals(:globex_user))

          get "/api/v1/sandbox/oauth_apps", headers: auth_headers(token)
        end

        assert_response :ok
      end

      test "rejects requests without a sandbox token" do
        get "/api/v1/sandbox/oauth_apps"
        assert_response :unauthorized
      end

      test "rejects expired sandbox tokens" do
        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          token = token_for(@proxy, now: (SandboxEntitlements::Jwt::DEFAULT_TTL_SECONDS + 1.hour).seconds.ago)

          get "/api/v1/sandbox/oauth_apps", headers: auth_headers(token)
        end

        assert_response :unauthorized
      end

      private

      def auth_headers(token)
        { "Authorization" => "Bearer #{token}" }
      end

      def token_for(proxy, now: Time.current)
        SandboxEntitlements::Jwt.encode_for_proxy(proxy, now: now)
      end

      def json_body
        JSON.parse(response.body)
      end

      def with_env(values)
        previous = values.keys.to_h { |key| [ key, ENV[key] ] }
        values.each do |key, value|
          value.nil? ? ENV.delete(key) : ENV[key] = value
        end
        yield
      ensure
        previous.each do |key, value|
          value.nil? ? ENV.delete(key) : ENV[key] = value
        end
      end
    end
  end
end
