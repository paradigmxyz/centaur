require "test_helper"

module Api
  module V1
    class BrokerCredentialsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body = JSON.parse(response.body)

      test "rejects requests without an API key" do
        get api_v1_broker_credentials_url(namespace: "acme")
        assert_response :unauthorized
      end

      test "index lists credentials in a namespace without token material" do
        get api_v1_broker_credentials_url(namespace: "acme"), headers: auth_headers
        assert_response :ok
        row = json_body.fetch("data").first
        assert_equal "bootstrapping", row["status"]
        refute row.key?("access_token")
        refute row.key?("refresh_token")
      end

      test "show returns config and status but never secret material" do
        bc = broker_credentials(:acme_managed_gmail)
        get api_v1_broker_credential_url(id: bc.oid), headers: auth_headers
        assert_response :ok
        data = json_body.fetch("data")
        assert_equal "https://oauth2.googleapis.com/token", data["token_endpoint"]
        assert_equal "gmail-client-id", data["client_id"]
        assert_equal "refresh_token", data["grant"]
        refute data.key?("client_secret")
        refute data.key?("username")
        refute data.key?("password")
        refute data.key?("api_key")
        refute data.key?("access_token")
        refute data.key?("refresh_token")
      end

      test "serializes oauth_app provenance for flow-minted credentials" do
        app = oauth_apps(:acme_google)
        cred = BrokerCredential.create!(namespace: "acme", foreign_id: "minted-serialize",
                                        token_endpoint: "https://oauth2.googleapis.com/token",
                                        oauth_app: app, provider_subject: "sub-ser",
                                        provider_email: "p@example.com", external_user_key: "user-ser")
        get api_v1_broker_credential_url(id: cred.oid), headers: auth_headers
        assert_response :ok
        data = json_body.fetch("data")
        assert_equal app.oid, data["oauth_app_id"]
        assert_equal "sub-ser", data["provider_subject"]
        assert_equal "p@example.com", data["provider_email"]
        assert_equal "user-ser", data["external_user_key"]
      end

      test "create seeds the refresh_token, schedules it due now, and redacts secrets" do
        body = {
          data: {
            namespace: "acme", foreign_id: "new-managed",
            token_endpoint: "https://idp.example/token",
            scopes: [ "x" ],
            client_id: "the-client-id",
            client_secret: "the-client-secret",
            refresh_token: "super-secret-seed",
            token_endpoint_headers: { "X-Api-Key" => "k" }
          }
        }

        assert_difference -> { BrokerCredential.count } => 1 do
          post api_v1_broker_credentials_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created
        data = json_body.fetch("data")
        assert_equal "the-client-id", data["client_id"]
        refute data.key?("client_secret")
        refute data.key?("refresh_token")
        assert_equal [ "X-Api-Key" ], data["token_endpoint_header_names"]
        assert data["next_attempt_at"].present?, "should be scheduled due"

        created = BrokerCredential.find_by_oid(data["id"])
        assert_equal "super-secret-seed", created.refresh_token # persisted + decryptable
        assert_equal "the-client-secret", created.client_secret
        assert_equal({ "X-Api-Key" => "k" }, created.token_endpoint_headers)
      end

      test "create password grant stores initial values and redacts secrets" do
        body = {
          data: {
            namespace: "acme", foreign_id: "password-provider",
            grant: "password",
            token_endpoint: "https://auth.example.com/token",
            client_id: "password-client",
            client_secret: "password-secret",
            username: "password-user",
            password: "password-value",
            token_endpoint_headers: { "x-api-key" => "password-key" }
          }
        }

        assert_difference -> { BrokerCredential.count } => 1 do
          post api_v1_broker_credentials_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created
        data = json_body.fetch("data")
        assert_equal "password", data["grant"]
        refute data.key?("client_secret")
        refute data.key?("username")
        refute data.key?("password")
        assert_equal [ "x-api-key" ], data["token_endpoint_header_names"]

        created = BrokerCredential.find_by_oid(data["id"])
        assert_equal "password-user", created.username
        assert_equal "password-value", created.password
        assert_equal "password-secret", created.client_secret
        assert_equal({ "x-api-key" => "password-key" }, created.token_endpoint_headers)
        assert created.next_attempt_at.present?
      end

      test "create preqin grant stores username and API key and redacts secrets" do
        body = {
          data: {
            namespace: "acme", foreign_id: "preqin",
            grant: "preqin",
            username: "preqin-user",
            api_key: "preqin-api-key"
          }
        }

        assert_difference -> { BrokerCredential.count } => 1 do
          post api_v1_broker_credentials_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created
        data = json_body.fetch("data")
        assert_equal "preqin", data["grant"]
        assert_equal BrokerCredential::PREQIN_TOKEN_ENDPOINT, data["token_endpoint"]
        assert_nil data["client_id"]
        refute data.key?("username")
        refute data.key?("api_key")

        created = BrokerCredential.find_by_oid(data["id"])
        assert_equal "preqin-user", created.username
        assert_equal "preqin-api-key", created.api_key
        assert created.next_attempt_at.present?
      end

      test "create rejects a missing client_id" do
        body = {
          data: {
            namespace: "acme", foreign_id: "incomplete",
            token_endpoint: "https://idp.example/token",
            client_secret: "sec"
          }
        }
        assert_no_difference -> { BrokerCredential.count } do
          post api_v1_broker_credentials_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "create password grant rejects missing password" do
        body = {
          data: {
            namespace: "acme", foreign_id: "password-incomplete",
            grant: "password",
            token_endpoint: "https://idp.example/token",
            client_id: "cid",
            username: "user"
          }
        }
        assert_no_difference -> { BrokerCredential.count } do
          post api_v1_broker_credentials_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "password").present?
      end

      test "create preqin grant rejects missing API key" do
        body = {
          data: {
            namespace: "acme", foreign_id: "preqin-incomplete",
            grant: "preqin",
            username: "preqin-user"
          }
        }
        assert_no_difference -> { BrokerCredential.count } do
          post api_v1_broker_credentials_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "api_key").present?
      end

      test "re-auth via PUT clears dead state and reschedules" do
        bc = broker_credentials(:acme_managed_gmail)
        bc.update!(dead: true, dead_reason: "invalid_grant", failure_count: 3)

        body = { data: { refresh_token: "fresh-seed" } }
        put api_v1_broker_credential_url(id: bc.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        bc.reload
        refute bc.dead?
        assert_nil bc.dead_reason
        assert_equal 0, bc.failure_count
        assert_equal "fresh-seed", bc.refresh_token
      end

      test "blank write-only fields preserve existing password grant material" do
        bc = BrokerCredential.create!(namespace: "acme", foreign_id: "password-preserve",
                                      grant: "password", token_endpoint: "https://idp.example/token",
                                      client_id: "cid", client_secret: "secret",
                                      username: "user", password: "pass", created_by: users(:acme_admin))

        body = { data: { client_secret: "", username: "", password: "" } }
        patch api_v1_broker_credential_url(id: bc.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        bc.reload
        assert_equal "secret", bc.client_secret
        assert_equal "user", bc.username
        assert_equal "pass", bc.password
      end

      test "password initial values update clears dead state and reschedules" do
        bc = BrokerCredential.create!(namespace: "acme", foreign_id: "password-dead",
                                      grant: "password", token_endpoint: "https://idp.example/token",
                                      client_id: "cid", username: "old", password: "old",
                                      dead: true, dead_reason: "invalid_grant", failure_count: 3,
                                      created_by: users(:acme_admin))

        body = { data: { username: "new-user", password: "new-pass" } }
        patch api_v1_broker_credential_url(id: bc.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        bc.reload
        assert_equal "new-user", bc.username
        assert_equal "new-pass", bc.password
        refute bc.dead?
        assert_nil bc.dead_reason
        assert_equal 0, bc.failure_count
        assert bc.next_attempt_at.present?
      end

      test "preqin API key update clears dead state and reschedules" do
        bc = BrokerCredential.create!(namespace: "acme", foreign_id: "preqin-dead",
                                      grant: "preqin", username: "old", api_key: "old",
                                      dead: true, dead_reason: "http_400", failure_count: 3,
                                      created_by: users(:acme_admin))

        body = { data: { api_key: "new-key" } }
        patch api_v1_broker_credential_url(id: bc.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        bc.reload
        assert_equal "new-key", bc.api_key
        refute bc.dead?
        assert_nil bc.dead_reason
        assert_equal 0, bc.failure_count
        assert bc.next_attempt_at.present?
      end

      test "destroy removes the credential" do
        bc = broker_credentials(:globex_managed_api)
        assert_difference -> { BrokerCredential.count } => -1 do
          delete api_v1_broker_credential_url(id: bc.oid), headers: auth_headers("iak_globex-ci-token")
        end
        assert_response :no_content
      end

      test "destroy is blocked with 409 while a token_broker source references it" do
        bc = broker_credentials(:globex_managed_api)
        SecretSource.create!(source_type: "token_broker",
                             config: { "credential_id" => bc.foreign_id, "credential_namespace" => bc.namespace })
        assert_no_difference -> { BrokerCredential.count } do
          delete api_v1_broker_credential_url(id: bc.oid), headers: auth_headers("iak_globex-ci-token")
        end
        assert_response :conflict
        assert_match "referenced by", json_body.dig("error", "message")
      end
    end
  end
end
