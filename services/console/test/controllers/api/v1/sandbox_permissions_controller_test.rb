require "test_helper"

module Api
  module V1
    class SandboxPermissionsControllerTest < ActionDispatch::IntegrationTest
      setup do
        @proxy = proxies(:acme_proxy)
        SecretSource.create!(
          source_type: "control_plane",
          secret: "s3cr3t-db-pass",
          static_secret: static_secrets(:db_password_replace)
        )
        SlackChannelPermission.create!(
          principal: @proxy.principal,
          channel_id: "C0123456789",
          channel_name: "general",
          upload_enabled: true,
          history_enabled: true
        )
      end

      test "returns redacted sandbox permissions for a valid sandbox token" do
        credential = BrokerCredential.create!(
          namespace: @proxy.principal.namespace,
          foreign_id: "google-personal",
          name: "Google - Personal User",
          token_endpoint: "https://oauth2.googleapis.com/token",
          oauth_app: oauth_apps(:acme_google),
          provider_email: "person@example.com",
          provider_subject: "google-sub-1",
          scopes: [ "https://www.googleapis.com/auth/gmail.readonly" ],
          refresh_token: "refresh-token",
          access_token: "access-token",
          expires_at: 1.hour.from_now,
          last_refresh: Time.current
        )
        secret = StaticSecret.new(
          namespace: @proxy.principal.namespace,
          name: "Google - Personal User token",
          broker_credential: credential,
          inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
        )
        secret.build_source(source_type: "token_broker", config: { "credential_id" => credential.oid })
        secret.rules.build(host: "www.googleapis.com", position: 0)
        secret.save!
        Grant.create!(principal: @proxy.principal, static_secret: secret, created_by: users(:acme_admin))

        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          get "/api/v1/sandbox/permissions", headers: auth_headers(token_for(@proxy))
        end
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal @proxy.name, data.fetch("sandbox_id")
        assert_equal @proxy.oid, data.fetch("proxy_id")
        assert_equal @proxy.principal.oid, data.fetch("principal_id")
        assert_equal @proxy.principal.namespace, data.dig("principal", "namespace")
        assert_equal @proxy.principal.sandbox_repo_cache, data.dig("capabilities", "sandbox_repo_cache")
        assert_equal 1, data.fetch("slack_channel_permissions").length
        assert_equal [
          {
            "id" => credential.oid,
            "oauth_app_id" => oauth_apps(:acme_google).oid,
            "slug" => "google",
            "provider" => "google",
            "provider_email" => "person@example.com",
            "provider_subject" => "google-sub-1",
            "status" => "live",
            "scopes" => [ "https://www.googleapis.com/auth/gmail.readonly" ]
          }
        ], data.fetch("oauth_credentials")

        entry = data.dig("permissions", "secrets").find { |secret| secret.dig("source", "type") == "control_plane" }
        refute_nil entry
        assert_equal "[redacted]", entry.dig("source", "value")
        refute_includes response.body, "s3cr3t-db-pass"
        assert_equal "no-store", response.headers["Cache-Control"]
        assert_match(/\A"[0-9a-f]{64}"\z/, response.headers["ETag"])
      end

      test "rejects requests without a sandbox token" do
        get "/api/v1/sandbox/permissions"
        assert_response :unauthorized
      end

      test "rejects tokens after proxy assignment changes" do
        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          token = token_for(@proxy)
          @proxy.update!(principal: principals(:globex_user))

          get "/api/v1/sandbox/permissions", headers: auth_headers(token)
        end

        assert_response :unauthorized
      end

      test "rejects expired sandbox tokens" do
        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          token = token_for(@proxy, now: (SandboxEntitlements::Jwt::DEFAULT_TTL_SECONDS + 1.hour).seconds.ago)

          get "/api/v1/sandbox/permissions", headers: auth_headers(token)
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
