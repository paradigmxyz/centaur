require "test_helper"

module Api
  module V1
    class OauthAppsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body = JSON.parse(response.body)

      def valid_body(**overrides)
        { data: {
          provider: "google", slug: "api-google", client_id: "the-client-id", client_secret: "the-secret",
          allowed_scopes: [ "https://www.googleapis.com/auth/gmail.readonly" ],
          credential_namespace: "acme"
        }.merge(overrides) }
      end

      test "rejects requests without an API key" do
        get api_v1_oauth_apps_url
        assert_response :unauthorized
      end

      test "index lists apps without the client_secret" do
        get api_v1_oauth_apps_url, headers: auth_headers
        assert_response :ok
        row = json_body.fetch("data").first
        assert_equal "google", row["provider"]
        refute row.key?("client_secret")
        refute row.key?("namespace")
        refute row.key?("foreign_id")
      end

      test "show returns config but never the client_secret" do
        app = oauth_apps(:acme_google)
        app.update!(client_secret: "shh")
        get api_v1_oauth_app_url(id: app.oid), headers: auth_headers
        assert_response :ok
        data = json_body.fetch("data")
        assert_equal "google", data["slug"]
        assert_equal "acme-google-client-id", data["client_id"]
        refute data.key?("client_secret")
      end

      test "lookup resolves by slug" do
        get lookup_api_v1_oauth_apps_url(slug: "google"), headers: auth_headers
        assert_response :ok
        assert_equal oauth_apps(:acme_google).oid, json_body.dig("data", "id")
      end

      test "create persists the app and redacts the secret" do
        assert_difference -> { OauthApp.count } => 1 do
          post api_v1_oauth_apps_url, params: valid_body.to_json, headers: auth_headers
        end
        assert_response :created
        data = json_body.fetch("data")
        assert_equal "google", data["provider"]
        assert_equal "api-google", data["slug"]
        refute data.key?("client_secret")

        created = OauthApp.find_by_oid(data["id"])
        assert_equal "the-secret", created.client_secret
        assert_equal "acme", created.credential_namespace
      end

      test "create rejects an unsupported provider" do
        assert_no_difference -> { OauthApp.count } do
          post api_v1_oauth_apps_url, params: valid_body(provider: "github").to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT upsert creates by slug then updates leaving a blank secret in place" do
        put api_v1_oauth_app_url(id: "put-google"),
            params: valid_body(slug: nil).to_json, headers: auth_headers
        assert_response :created
        app = OauthApp.find_by!(slug: "put-google")
        assert_equal "the-secret", app.client_secret

        # A second PUT without a client_secret keeps the stored value.
        put api_v1_oauth_app_url(id: "put-google"),
            params: { data: { description: "Renamed", client_secret: "" } }.to_json, headers: auth_headers
        assert_response :ok
        app.reload
        assert_equal "Renamed", app.description
        assert_equal "the-secret", app.client_secret
      end

      test "destroy removes an app with no minted credentials" do
        app = oauth_apps(:acme_google_disabled)
        assert_difference -> { OauthApp.count } => -1 do
          delete api_v1_oauth_app_url(id: app.oid), headers: auth_headers
        end
        assert_response :no_content
      end

      test "destroy is blocked with 409 while minted credentials exist" do
        app = oauth_apps(:acme_google)
        BrokerCredential.create!(namespace: "acme", foreign_id: "minted-api",
                                 token_endpoint: "https://oauth2.googleapis.com/token",
                                 oauth_app: app, provider_subject: "sub-api")
        assert_no_difference -> { OauthApp.count } do
          delete api_v1_oauth_app_url(id: app.oid), headers: auth_headers
        end
        assert_response :conflict
      end
    end
  end
end
