require "test_helper"

module Api
  module V1
    # GET /api/v1/integrations, ported from the former HTML
    # console/integrations_controller_test.rb: what each user sees, their
    # connection state, and that shared apps and presets stay admin-only.
    class ConsoleIntegrationsTest < ActionDispatch::IntegrationTest
      def apps = json_body.dig("data", "apps")
      def app_entry(slug) = apps.find { |a| a["slug"] == slug }

      def google_credential(**attrs)
        BrokerCredential.create!({
          oauth_app: oauth_apps(:acme_google), token_endpoint: "https://oauth2.googleapis.com/token",
          client_id: "google-client-id"
        }.merge(attrs))
      end

      test "integrations are open to any active user" do
        assert_active_user_api :get, api_v1_integrations_path
        sign_in_console(users(:member_user))
        get api_v1_integrations_path
        assert_response :ok
        get api_v1_integrations_path, headers: bearer_headers(MEMBER_TOKEN)
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
      end

      test "shared apps coexist with personal apps and only admins see their shared connection" do
        app = oauth_apps(:acme_google).dup
        app.update!(slug: "shared-google", client_secret: "synthetic", shared: true)
        BrokerCredential.create!(oauth_app: app, provider_subject: "shared-user", token_endpoint: app.provider_strategy.token_endpoint,
          created_by: users(:globex_admin))

        sign_in_console(users(:member_user))
        get api_v1_integrations_path
        assert_nil app_entry("shared-google")
        assert_equal "http://www.example.com/oauth/google/start", app_entry("google")["start_url"]
        assert_equal [], json_body.dig("data", "presets")

        sign_in_console(users(:acme_admin))
        get api_v1_integrations_path
        shared = app_entry("shared-google")
        assert_equal app.oid, shared["id"]
        assert_equal "shared", shared["usage"]
        assert_equal "connected", shared.dig("connection", "status")
        assert_equal "http://www.example.com/oauth/shared-google/start", shared["start_url"]
        assert_equal [ { "provider" => "mercator", "display_name" => "Mercator", "description" => "Shared with your agents." } ],
                     json_body.dig("data", "presets")
      end

      test "a non-admin sees enabled apps with their start links and providers, and no disabled apps" do
        sign_in_console(users(:member_user))
        get api_v1_integrations_path
        assert_response :ok

        %w[google slack github].each do |slug|
          assert_equal "http://www.example.com/oauth/#{slug}/start", app_entry(slug)["start_url"]
        end
        assert_nil app_entry("google-disabled")
        assert_equal "google", app_entry("google")["provider"]
        assert_equal "slack", app_entry("slack")["provider"]
        assert_equal apps.pluck("slug").sort, apps.pluck("slug")

        # Never the OAuth client or other credential material.
        apps.each { |a| assert_empty a.keys & %w[client_id client_secret client_secret_set labels allowed_scopes] }
        assert_not_includes response.body, "acme-google-client-id"
      end

      test "an app already connected under the user's email shows its status" do
        sign_in_console(users(:member_user))
        credential = google_credential(foreign_id: "google-google-member-sub", name: "Google – Member",
          provider_subject: "member-sub", provider_email: users(:member_user).email, external_user_key: "member-key")

        get api_v1_integrations_path
        assert_equal({ "status" => "connected" }, app_entry("google")["connection"])
        assert_nil app_entry("slack")["connection"]

        # A dead credential asks the user to reconnect rather than claiming success.
        credential.update!(dead: true, dead_reason: "invalid_grant")
        get api_v1_integrations_path
        assert_equal "needs_reconnect", app_entry("google").dig("connection", "status")
      end

      test "a credential the user minted shows connected even when the provider email differs" do
        sign_in_console(users(:member_user))
        google_credential(foreign_id: "google-google-personal-sub", name: "Google – Personal", provider_subject: "personal-sub",
          provider_email: "personal@gmail.example", external_user_key: "personal-key", created_by: users(:member_user))

        get api_v1_integrations_path
        assert_equal "connected", app_entry("google").dig("connection", "status")
      end

      test "a Slack credential without search scope asks the user to reconnect" do
        sign_in_console(users(:member_user))
        credential = BrokerCredential.create!(
          oauth_app: oauth_apps(:acme_slack), foreign_id: "slack-slack-member-sub", name: "Slack – Member",
          token_endpoint: "https://slack.com/api/oauth.v2.access", provider_subject: "U-MEMBER",
          provider_email: users(:member_user).email, external_user_key: "slack-member-key", scopes: %w[chat:write]
        )

        get api_v1_integrations_path
        assert_equal "needs_reconnect", app_entry("slack").dig("connection", "status")

        credential.update!(scopes: %w[chat:write search:read])
        get api_v1_integrations_path
        assert_equal "connected", app_entry("slack").dig("connection", "status")
      end

      test "a credential minted for someone else's email does not mark the app connected" do
        sign_in_console(users(:member_user))
        google_credential(foreign_id: "google-google-other-sub", name: "Google – Other", provider_subject: "other-sub",
          provider_email: users(:acme_admin).email, external_user_key: "other-key")

        get api_v1_integrations_path
        assert_nil app_entry("google")["connection"]
        assert apps.all? { |a| a["connection"].nil? }
      end

      test "an integration that is not always available discloses DM-only use" do
        sign_in_console(users(:member_user))
        get api_v1_integrations_path
        assert apps.all? { |a| a["usage"] == "direct_messages" }
      end

      test "an always-available integration discloses automatic use" do
        oauth_apps(:acme_google).update!(client_secret: "shh", always_available: true)
        sign_in_console(users(:member_user))
        get api_v1_integrations_path
        assert_equal "always_available", app_entry("google")["usage"]
        assert_equal "direct_messages", app_entry("slack")["usage"]
      end

      test "an admin sees the same apps" do
        sign_in_console(users(:acme_admin))
        get api_v1_integrations_path
        assert_response :ok
        assert_equal "http://www.example.com/oauth/google/start", app_entry("google")["start_url"]
      end

      test "the SSH key card reports whether the user saved a key" do
        sign_in_console(users(:member_user))
        get api_v1_integrations_path
        assert_equal({ "configured" => false }, json_body.dig("data", "ssh_key"))

        private_key, = synthetic_ssh_key
        users(:member_user).create_personal_ssh_key!(name: "My SSH key", created_by: users(:member_user), private_key: private_key)
        get api_v1_integrations_path
        assert_equal({ "configured" => true }, json_body.dig("data", "ssh_key"))
        assert_not_includes response.body, "PRIVATE KEY"

        # Another user's key never counts.
        sign_in_console(users(:acme_admin))
        get api_v1_integrations_path
        assert_equal({ "configured" => false }, json_body.dig("data", "ssh_key"))
      end
    end
  end
end
