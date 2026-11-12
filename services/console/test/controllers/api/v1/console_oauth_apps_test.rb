require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does.
    class ConsoleOauthAppsTest < ActionDispatch::IntegrationTest
      setup do
        @operator = users(:acme_admin)
        sign_in_console(@operator)
      end

      teardown do
        OauthAppsController.registration_client_factory = -> { Oauth::DynamicRegistration.new }
      end

      def registration_client(&block)
        OauthAppsController.registration_client_factory = -> {
          Oauth::DynamicRegistration.new(http: HttpClient.new(http: block))
        }
      end

      def write(method, path, data)
        public_send(method, path, params: { data: data }, headers: csrf_headers, as: :json)
      end

      def form_body(**overrides)
        { slug: "new-google", description: "New Google integration", provider: "google",
          client_id: "cid", client_secret: "shh", enabled: true, always_available: false, shared: false,
          allowed_scopes: %w[https://www.googleapis.com/auth/gmail.readonly], provider_config: {}, labels: {} }.merge(overrides)
      end

      test "app endpoints are admin-only for every caller type" do
        app = oauth_apps(:acme_google)
        registration_client { |**| flunk "Must not register" }
        assert_admin_only_api :get, api_v1_oauth_apps_path
        assert_admin_only_api :get, api_v1_oauth_app_path(app.oid)
        assert_admin_only_api :post, api_v1_oauth_apps_path, params: { data: form_body }
        assert_admin_only_api :patch, api_v1_oauth_app_path(app.oid), params: { data: { description: "x" } }
        assert_admin_only_api :get, credentials_api_v1_oauth_app_path(app.oid)
        assert_admin_only_api :get, provider_details_api_v1_oauth_app_path(app.oid)
        assert_admin_only_api :get, api_v1_oauth_providers_path
        assert_admin_only_api :post, preset_api_v1_oauth_apps_path(provider: "mercator")
        assert_not OauthApp.exists?(slug: "new-google")
        assert_not OauthApp.exists?(slug: "mercator")
        assert_equal "Acme Google integration", app.reload.description
      end

      test "oauth apps list carries provider, client id, and minted credential counts" do
        app = oauth_apps(:acme_google)
        BrokerCredential.create!(oauth_app: app, provider_subject: "s1", token_endpoint: "https://oauth2.googleapis.com/token")
        BrokerCredential.create!(oauth_app: app, provider_subject: "s2", token_endpoint: "https://oauth2.googleapis.com/token")
        get api_v1_oauth_apps_path(limit: 200)
        assert_response :ok
        row = json_body["data"].find { |a| a["id"] == app.oid }
        assert_equal "google", row["provider"]
        assert_equal "acme-google-client-id", row["client_id"]
        assert_equal 2, row["credentials_count"]
        assert_equal 0, json_body["data"].find { |a| a["slug"] == "slack" }["credentials_count"]
        assert_equal OauthApp.order(created_at: :asc, id: :asc).map(&:oid), json_body["data"].pluck("id")
      end

      test "oauth app detail shows config, the redirect URI, and a start URL but never the secret" do
        app = oauth_apps(:acme_google)
        app.update!(client_secret: "shh")
        get api_v1_oauth_app_path(app.oid)
        assert_response :ok
        data = json_body["data"]
        assert_equal app.slug, data["slug"]
        assert_equal app.client_id, data["client_id"]
        assert_equal true, data["client_secret_set"]
        assert_equal "http://www.example.com/oauth/google/callback", data["redirect_uri"]
        assert_equal "http://www.example.com/oauth/google/start", data["start_url"]
        assert_not data.key?("client_secret")
        assert_not_includes response.body, "shh"
      end

      test "the redirect and start URLs use the configured public URL" do
        ConsoleEnv.stub(:[], ->(key) { key == "PUBLIC_URL" ? "https://console.example.test" : nil }) do
          get api_v1_oauth_app_path(oauth_apps(:acme_google).oid)
        end
        assert_equal "https://console.example.test/oauth/google/callback", json_body.dig("data", "redirect_uri")
        assert_equal "https://console.example.test/oauth/google/start", json_body.dig("data", "start_url")
      end

      test "oauth app detail 404s for an unknown id" do
        get api_v1_oauth_app_path("oap_missing")
        assert_response :not_found
        get credentials_api_v1_oauth_app_path("oap_missing")
        assert_response :not_found
        get provider_details_api_v1_oauth_app_path("oap_missing")
        assert_response :not_found
      end

      test "minted credentials list the app's credentials oldest first without token material" do
        app = oauth_apps(:acme_google)
        first = BrokerCredential.create!(oauth_app: app, foreign_id: "minted-1", provider_subject: "s1",
          provider_email: "person@example.com", external_user_key: "user-1",
          token_endpoint: "https://oauth2.googleapis.com/token", access_token: "synthetic-access")
        second = BrokerCredential.create!(oauth_app: app, provider_subject: "s2", token_endpoint: "https://oauth2.googleapis.com/token")
        BrokerCredential.create!(oauth_app: oauth_apps(:acme_slack), provider_subject: "s3", token_endpoint: "https://slack.com/api/oauth.v2.access")

        get credentials_api_v1_oauth_app_path(app.oid)
        assert_response :ok
        assert_equal [ first.oid, second.oid ], json_body["data"].pluck("id")
        row = json_body["data"].first
        assert_equal "minted-1", row["foreign_id"]
        assert_equal first.status, row["status"]
        assert_equal "person@example.com", row["provider_email"]
        assert_equal "user-1", row["external_user_key"]
        assert_equal 2, json_body.dig("meta", "total")
        assert_not_includes response.body, "synthetic-access"

        get credentials_api_v1_oauth_app_path(oauth_apps(:acme_github).oid)
        assert_equal [], json_body["data"]
      end

      test "providers list the registry with the custom provider's config keys" do
        get api_v1_oauth_providers_path
        assert_response :ok
        keys = json_body["data"].pluck("key")
        assert_equal Oauth::Providers.keys, keys
        custom = json_body["data"].find { |p| p["key"] == "custom" }
        assert_equal Oauth::Providers::Custom::CONFIG_KEYS, custom["config_keys"]
        assert_equal %w[userinfo_endpoint], custom["optional_config_keys"]
        assert_equal [], json_body["data"].find { |p| p["key"] == "google" }["config_keys"]
        assert json_body["data"].find { |p| p["key"] == "mercator" }["presettable"]
      end

      test "shared mode is set on create and kept on edit" do
        template = oauth_apps(:acme_google)
        write :post, api_v1_oauth_apps_path, form_body(slug: "shared-google", client_id: template.client_id,
          client_secret: "synthetic", shared: true, allowed_scopes: template.allowed_scopes)
        assert_response :created
        app = OauthApp.find_by!(slug: "shared-google")
        assert app.shared?
        assert_equal app.oid, json_body.dig("data", "id")

        # The edit form omits `shared` (it is fixed after creation).
        write :patch, api_v1_oauth_app_path(app.oid), form_body(slug: app.slug, client_id: app.client_id,
          allowed_scopes: app.allowed_scopes).except(:shared, :client_secret)
        assert_response :ok
        assert app.reload.shared?

        write :patch, api_v1_oauth_app_path(app.oid), { shared: false }
        assert_response :unprocessable_entity
        assert_includes json_body.dig("error", "details", "shared"), "cannot be changed after creation"
        assert app.reload.shared?
      end

      test "create builds an app with all fields" do
        assert_difference -> { OauthApp.count } => 1 do
          write :post, api_v1_oauth_apps_path, form_body(
            always_available: true, labels: { team: "comms" },
            allowed_scopes: %w[https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly]
          )
        end
        assert_response :created

        app = OauthApp.find_by!(slug: "new-google")
        assert_equal app.oid, json_body.dig("data", "id")
        assert_equal "google", app.provider
        assert_equal "cid", app.client_id
        assert_equal "shh", app.client_secret
        assert_equal "New Google integration", app.description
        assert_equal %w[https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly], app.allowed_scopes
        assert_equal({ "team" => "comms" }, app.labels)
        assert app.enabled?
        assert app.always_available?
        assert_not app.has_attribute?(:credential_namespace)
        assert_equal @operator, app.created_by
        assert_not json_body["data"].key?("client_secret")
        assert_equal true, json_body.dig("data", "client_secret_set")
      end

      test "create stores the custom provider config and other providers take an empty one" do
        config = {
          authorization_endpoint: "https://auth.example.test/authorize", token_endpoint: "https://auth.example.test/token",
          userinfo_endpoint: "https://auth.example.test/userinfo", resource: "https://api.example.test/mcp"
        }
        write :post, api_v1_oauth_apps_path, form_body(slug: "custom-app", provider: "custom", allowed_scopes: %w[read],
                                                        provider_config: config)
        assert_response :created
        app = OauthApp.find_by!(slug: "custom-app")
        assert_equal config.stringify_keys, app.provider_config

        # The SPA drops blank config fields, so a cleared endpoint is reported missing.
        write :patch, api_v1_oauth_app_path(app.oid), { provider: "custom", provider_config: config.except(:token_endpoint) }
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "provider_config").any? { |m| m.include?("token_endpoint") }
        assert_equal "https://auth.example.test/token", app.reload.provider_config["token_endpoint"]

        # Non-custom providers are sent an empty config (the SPA drops the fields).
        write :post, api_v1_oauth_apps_path, form_body(slug: "google-app", allowed_scopes: %w[read], provider_config: {})
        assert_response :created
        assert_empty OauthApp.find_by!(slug: "google-app").provider_config

        write :post, api_v1_oauth_apps_path, form_body(slug: "google-app-2", allowed_scopes: %w[read], provider_config: config)
        assert_response :unprocessable_entity
        assert_not OauthApp.exists?(slug: "google-app-2")
      end

      test "create with an invalid provider is rejected without writing" do
        assert_no_difference -> { OauthApp.count } do
          write :post, api_v1_oauth_apps_path, form_body(provider: "unsupported", slug: "bad-provider", allowed_scopes: %w[a])
        end
        assert_response :unprocessable_entity
        assert_includes json_body.dig("error", "details", "provider"), "is not a supported provider"
      end

      test "create with no allowed scopes is rejected" do
        assert_no_difference -> { OauthApp.count } do
          write :post, api_v1_oauth_apps_path, form_body(slug: "no-scopes", allowed_scopes: [])
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "allowed_scopes").present?
      end

      test "create without a client secret is rejected" do
        assert_no_difference -> { OauthApp.count } do
          write :post, api_v1_oauth_apps_path, form_body(client_secret: "")
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "client_secret").present?
      end

      test "update changes attributes and replaces lists" do
        app = oauth_apps(:acme_google)
        app.update!(client_secret: "original")
        write :patch, api_v1_oauth_app_path(app.oid), form_body(
          slug: app.slug, description: "Renamed", client_id: "new-cid", enabled: false,
          allowed_scopes: %w[https://www.googleapis.com/auth/gmail.readonly]
        ).except(:shared, :client_secret)
        assert_response :ok
        app.reload
        assert_equal "Renamed", app.description
        assert_equal "new-cid", app.client_id
        refute app.enabled?
        assert_equal %w[https://www.googleapis.com/auth/gmail.readonly], app.allowed_scopes
        assert_equal({}, app.labels)
      end

      test "update sets and clears always_available" do
        app = oauth_apps(:acme_google)
        app.update!(client_secret: "original")
        base = form_body(slug: app.slug, client_id: app.client_id, allowed_scopes: app.allowed_scopes).except(:shared, :client_secret)

        write :patch, api_v1_oauth_app_path(app.oid), base.merge(always_available: true)
        assert_response :ok
        assert app.reload.always_available?

        # The SPA always sends the checkbox state, so unchecking clears the flag.
        write :patch, api_v1_oauth_app_path(app.oid), base.merge(always_available: false)
        assert_response :ok
        refute app.reload.always_available?
      end

      test "update with a blank or omitted client_secret keeps the stored value" do
        app = oauth_apps(:acme_google)
        app.update!(client_secret: "original-secret")
        base = form_body(slug: app.slug, client_id: app.client_id, allowed_scopes: app.allowed_scopes).except(:shared)

        write :patch, api_v1_oauth_app_path(app.oid), base.merge(client_secret: "")
        assert_response :ok
        assert_equal "original-secret", app.reload.client_secret

        write :patch, api_v1_oauth_app_path(app.oid), base.except(:client_secret)
        assert_response :ok
        assert_equal "original-secret", app.reload.client_secret

        write :patch, api_v1_oauth_app_path(app.oid), base.merge(client_secret: "rotated")
        assert_equal "rotated", app.reload.client_secret
        assert_not_includes response.body, "rotated"
      end

      test "preset registers once even with existing manual credentials and then shows manage" do
        BrokerCredential.create!(token_endpoint: "https://mercator.sh/oauth/token", client_id: "manual")
        registration_client do |**request|
          assert_equal [ "http://www.example.com/oauth/mercator/callback" ], JSON.parse(request[:body])["redirect_uris"]
          HttpClient::Response.new(status: 201, body: { client_id: "id", client_secret: "synthetic" }.to_json)
        end
        assert_difference("OauthApp.count", 1) { post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf_headers }
        assert_response :ok
        app = OauthApp.find_by!(slug: "mercator")
        assert app.shared?
        assert_equal "http://www.example.com/oauth/mercator/start", json_body.dig("data", "start_url")
        assert_equal app.oid, json_body.dig("data", "id")
        assert_not_includes response.body, "synthetic"

        registration_client { |**| flunk "Must reuse existing app" }
        assert_no_difference("OauthApp.count") { post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf_headers }
        assert_equal "http://www.example.com/oauth/mercator/start", json_body.dig("data", "start_url")

        BrokerCredential.create!(oauth_app: app, provider_subject: "0x#{'ab' * 20}", token_endpoint: app.provider_strategy.token_endpoint)
        get api_v1_integrations_path
        entry = json_body.dig("data", "apps").find { |a| a["slug"] == "mercator" }
        assert_equal app.oid, entry["id"]
        assert entry["shared"]
        # A connection exists, so the card offers Manage/Reconnect rather than Connect.
        assert entry["connection"].present?
        assert_not_includes json_body.dig("data", "presets").pluck("provider"), "mercator"
      end

      test "registration scopes use spaces even when consent scopes use commas" do
        provider = Oauth::Providers.fetch("mercator")
        preset = provider.preset.merge(allowed_scopes: %w[read write])
        registration_client do |**request|
          assert_equal "read write", JSON.parse(request[:body])["scope"]
          HttpClient::Response.new(status: 201, body: { client_id: "id", client_secret: "synthetic" }.to_json)
        end
        provider.stub(:preset, preset) do
          provider.stub(:scope_separator, ",") do
            post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf_headers
          end
        end
        assert_response :ok
        assert_equal "http://www.example.com/oauth/mercator/start", json_body.dig("data", "start_url")
      end

      test "preset reuses the winner of a concurrent unique slug conflict" do
        provider = Oauth::Providers::Mercator.new
        winner = OauthApp.create!(provider.preset.merge(provider: "mercator", client_id: "winner", client_secret: "synthetic", created_by: @operator))
        csrf = csrf_headers
        lookups = 0
        lookup = ->(*) { lookups += 1; lookups == 1 ? nil : winner }
        registration_client { |**| HttpClient::Response.new(status: 201, body: { client_id: "loser", client_secret: "synthetic" }.to_json) }
        OauthApp.stub(:find_by, lookup) do
          OauthApp.stub(:create!, ->(*) { raise ActiveRecord::RecordNotUnique }) do
            post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf
          end
        end
        assert_response :ok
        assert_equal "http://www.example.com/oauth/mercator/start", json_body.dig("data", "start_url")
        assert_equal "winner", winner.reload.client_id
      end

      test "a preset that conflicts with an existing app is refused" do
        OauthApp.create!(slug: "mercator", provider: "mercator", client_id: "id", client_secret: "synthetic",
                         allowed_scopes: [ "mercator:tools" ], shared: false, created_by: @operator)
        registration_client { |**| flunk "Must not register" }
        post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf_headers
        assert_response :conflict
        assert_equal "The preset conflicts with an existing OAuth app. Review its configuration.", json_body.dig("error", "message")
      end

      test "registration failure leaves no app and reports the error" do
        registration_client { |**| raise Timeout::Error }
        assert_no_difference("OauthApp.count") { post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf_headers }
        assert_response :bad_gateway
        assert_equal "Could not set up the integration. Please try again or review the OAuth app configuration.",
                     json_body.dig("error", "message")
      end

      test "non admins and unsupported providers cannot register" do
        registration_client { |**| flunk "Must not register" }
        post preset_api_v1_oauth_apps_path(provider: "google"), headers: csrf_headers
        assert_response :not_found
        post preset_api_v1_oauth_apps_path(provider: "nope"), headers: csrf_headers
        assert_response :not_found

        sign_in_console(users(:member_user))
        post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: csrf_headers
        assert_response :forbidden
        assert_not OauthApp.exists?(slug: "mercator")
      end

      def mercator_app
        @provider = Oauth::Providers.fetch("mercator")
        @app = OauthApp.create!(@provider.preset.merge(provider: "mercator", client_id: "id", client_secret: "synthetic", created_by: @operator))
        @credential = BrokerCredential.create!(oauth_app: @app, provider_subject: "0x#{'ab' * 20}",
          token_endpoint: @provider.token_endpoint, access_token: "synthetic", expires_at: 1.hour.from_now)
      end

      test "claim state is returned only for the app page and failures keep the link" do
        mercator_app
        [ true, false, nil ].each do |claimed|
          http = expect_http_call(status: claimed.nil? ? 500 : 200, body: { claimed: claimed, wallet_address: @credential.provider_subject }.to_json)
          client = HttpClient.new(http: http)
          HttpClient.stub(:new, client) { get provider_details_api_v1_oauth_app_path(@app.oid) }
          assert_response :ok
          data = json_body["data"]
          assert_equal "mercator", data["provider"]
          assert_equal @credential.oid, data.dig("credential", "id")
          assert_equal @credential.provider_subject, data.dig("credential", "provider_subject")
          claimed.nil? ? assert_nil(data.dig("details", "claimed")) : assert_equal(claimed, data.dig("details", "claimed"))
          assert_not_includes response.body, "synthetic"
          http.verify
        end
        @provider.stub(:details_for, ->(*) { flunk "No details on Integrations" }) { get api_v1_integrations_path }
        assert_response :ok
      end

      test "expired or dead credentials never fetch details or refresh" do
        mercator_app
        [ { expires_at: 1.minute.ago }, { expires_at: 1.hour.from_now, dead: true } ].each do |attrs|
          @credential.update!(attrs)
          @provider.stub(:details_for, ->(*) { flunk "No details for expired or dead credentials" }) do
            get provider_details_api_v1_oauth_app_path(@app.oid)
          end
          assert_response :ok
          assert_equal @credential.oid, json_body.dig("data", "credential", "id")
          assert_equal({}, json_body.dig("data", "details"))
        end
      end

      test "unexpected provider failure does not break the page" do
        mercator_app
        @provider.stub(:details_for, ->(*) { raise TypeError }) { get provider_details_api_v1_oauth_app_path(@app.oid) }
        assert_response :ok
        assert_equal({}, json_body.dig("data", "details"))
        assert_equal @credential.oid, json_body.dig("data", "credential", "id")
      end

      test "the wrapping static secret is linked for agent access" do
        mercator_app
        secret = StaticSecret.create!(name: "mercator-wallet", broker_credential: @credential,
                                      inject_config: { "header" => "Authorization" })
        @provider.stub(:details_for, ->(*) { { claimed: false } }) { get provider_details_api_v1_oauth_app_path(@app.oid) }
        assert_equal secret.oid, json_body.dig("data", "credential", "static_secret_id")
      end

      test "personal apps have no provider account to describe" do
        get provider_details_api_v1_oauth_app_path(oauth_apps(:acme_google).oid)
        assert_response :ok
        assert_equal({ "provider" => "google", "credential" => nil, "details" => {} }, json_body["data"])
      end
    end
  end
end
