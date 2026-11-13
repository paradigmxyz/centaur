require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does.
    class ConsoleCredentialsTest < ActionDispatch::IntegrationTest
      POLICY = {
        early_refresh_fraction: 0.5, early_refresh_slack_seconds: 120,
        max_refresh_interval_seconds: 3600, refresh_timeout_seconds: 10
      }.freeze

      setup do
        @operator = users(:acme_admin)
        sign_in_console(@operator)
      end

      def create_credential(data)
        post api_v1_console_broker_credentials_path, params: { data: data }, headers: csrf_headers, as: :json
      end

      def update_credential(cred, data)
        patch api_v1_console_broker_credential_path(cred.oid), params: { data: data }, headers: csrf_headers, as: :json
      end

      test "credential endpoints are admin-only for every caller type" do
        cred = broker_credentials(:globex_managed_api)
        assert_admin_only_api :get, api_v1_console_broker_credentials_path
        assert_admin_only_api :get, api_v1_console_broker_credential_path(cred.oid)
        assert_admin_only_api :post, api_v1_console_broker_credentials_path,
                              params: { data: { foreign_id: "sneaky", token_endpoint: "https://idp.example/token", client_id: "c" } }
        assert_admin_only_api :patch, api_v1_console_broker_credential_path(cred.oid), params: { data: { name: "sneaky" } }
        assert_admin_only_api :delete, api_v1_console_broker_credential_path(cred.oid)

        assert BrokerCredential.exists?(cred.id)
        assert_not BrokerCredential.exists?(foreign_id: "sneaky")
        assert_equal "managed-api", cred.reload.name
      end

      test "an unknown credential oid is a 404" do
        get api_v1_console_broker_credential_path("bcr_missing")
        assert_response :not_found
        delete api_v1_console_broker_credential_path("bcr_missing"), headers: csrf_headers
        assert_response :not_found
      end

      test "index lists credentials with id, status, provenance, and no token material" do
        app = oauth_apps(:acme_google)
        minted = BrokerCredential.create!(foreign_id: "minted-index", token_endpoint: "https://oauth2.googleapis.com/token",
                                          oauth_app: app, external_user_key: "user-index")

        get api_v1_console_broker_credentials_path(limit: 200)
        assert_response :ok
        rows = json_body["data"].index_by { |row| row["id"] }

        gmail = rows.fetch(broker_credentials(:acme_managed_gmail).oid)
        assert_equal "managed-gmail", gmail["foreign_id"]
        assert_equal "bootstrapping", gmail["status"]
        assert_nil gmail["oauth_app"]
        assert_equal BrokerCredential.count, json_body.dig("meta", "total")

        assert_equal({ "id" => app.oid, "slug" => app.slug, "provider" => app.provider }, rows.fetch(minted.oid)["oauth_app"])
        %w[access_token refresh_token client_secret username password api_key token_endpoint_headers].each do |key|
          assert_not gmail.key?(key), key
        end
      end

      test "index orders credentials by creation time" do
        get api_v1_console_broker_credentials_path(limit: 200)
        ids = json_body["data"].map { |row| row["id"] }
        assert_equal BrokerCredential.order(:created_at, :id).map(&:oid), ids
      end

      test "show returns refresh and client data but never token material" do
        cred = broker_credentials(:acme_managed_gmail)
        cred.update!(client_secret: "client-secret-value", refresh_token: "refresh-token-value",
                     token_endpoint_headers: { "Authorization" => "Basic header-value" })

        get api_v1_console_broker_credential_path(cred.oid)
        assert_response :ok
        data = json_body["data"]
        assert_equal cred.name, data["name"]
        assert_equal cred.client_id, data["effective_client_id"]
        assert_equal cred.token_endpoint, data["token_endpoint"]
        assert data.key?("next_attempt_at")
        assert_equal [ "Authorization" ], data["token_endpoint_header_names"]
        assert data["has_client_secret"]
        assert data["has_refresh_token"]
        assert_not data["has_username"]
        assert_not data["has_password"]
        assert_not data["has_api_key"]
        %w[client-secret-value refresh-token-value header-value].each do |plaintext|
          assert_not_includes response.body, plaintext
        end
        assert_no_match(/access[_ ]token/i, response.body)
      end

      test "show reports which write-only values are stored" do
        cred = BrokerCredential.create!(foreign_id: "password-flags", grant: "password",
                                        token_endpoint: "https://idp.example/token", client_id: "cid",
                                        username: "user-value", password: "password-value")
        get api_v1_console_broker_credential_path(cred.oid)
        data = json_body["data"]
        assert data["has_username"]
        assert data["has_password"]
        assert_not data["has_client_secret"]
        assert_not data["has_refresh_token"]
        assert_not_includes response.body, "user-value"
        assert_not_includes response.body, "password-value"
      end

      test "has_refresh_token reports a stored token without returning it, in index and show" do
        cred = broker_credentials(:globex_managed_api)
        cred.update!(refresh_token: "stored-refresh-token-value", access_token: "minted-access-token-value")

        get api_v1_console_broker_credential_path(cred.oid)
        assert_response :ok
        assert json_body.dig("data", "has_refresh_token")
        assert_not json_body["data"].key?("refresh_token")
        assert_not_includes response.body, "stored-refresh-token-value"
        assert_not_includes response.body, "minted-access-token-value"

        get api_v1_console_broker_credentials_path(limit: 200)
        assert_response :ok
        row = json_body["data"].find { |r| r["id"] == cred.oid }
        assert row["has_refresh_token"]
        assert_not row.key?("refresh_token")
        assert_not_includes response.body, "stored-refresh-token-value"
        assert_not_includes response.body, "minted-access-token-value"

        cred.update!(refresh_token: nil)
        get api_v1_console_broker_credential_path(cred.oid)
        assert_equal false, json_body.dig("data", "has_refresh_token")
      end

      test "show links the static secret that makes the credential grantable" do
        cred = broker_credentials(:acme_managed_gmail)
        secret = static_secrets(:acme_managed_gmail_secret)
        get api_v1_console_broker_credential_path(cred.oid)
        assert_equal({ "id" => secret.oid, "foreign_id" => secret.foreign_id, "name" => secret.name },
                     json_body.dig("data", "static_secret"))

        get api_v1_console_broker_credential_path(broker_credentials(:globex_managed_api).oid)
        assert_nil json_body.dig("data", "static_secret")
      end

      test "show includes the provider identity and inherited client id for a flow-minted credential" do
        app = oauth_apps(:acme_google)
        cred = BrokerCredential.create!(foreign_id: "minted-1",
                                        token_endpoint: "https://oauth2.googleapis.com/token",
                                        oauth_app: app, provider_subject: "sub-9",
                                        provider_email: "person@example.com", external_user_key: "user-9")
        get api_v1_console_broker_credential_path(cred.oid)
        data = json_body["data"]
        assert_equal "person@example.com", data["provider_email"]
        assert_equal "sub-9", data["provider_subject"]
        assert_equal "user-9", data["external_user_key"]
        assert_equal app.oid, data.dig("oauth_app", "id")
        assert_equal app.slug, data.dig("oauth_app", "slug")
        assert_nil data["client_id"]
        assert_equal app.client_id, data["effective_client_id"]
      end

      test "create builds a credential with config, scopes, headers, labels, and a seed" do
        assert_difference -> { BrokerCredential.count } => 1 do
          create_credential(POLICY.merge(
            foreign_id: "new-managed", name: "new-managed",
            token_endpoint: "https://idp.example/token", client_id: "cid",
            client_secret: "shhh", scopes: %w[scope.a scope.b], refresh_token: "seed-token",
            token_endpoint_headers: { "Authorization" => "Basic abc" }, labels: { "team" => "comms" }
          ))
        end
        assert_response :created

        cred = BrokerCredential.find_by!(foreign_id: "new-managed")
        assert_equal cred.oid, json_body.dig("data", "id")
        assert_equal %w[scope.a scope.b], cred.scopes
        assert_equal({ "Authorization" => "Basic abc" }, cred.token_endpoint_headers)
        assert_equal({ "team" => "comms" }, cred.labels)
        assert_equal "shhh", cred.client_secret
        assert_equal "seed-token", cred.refresh_token
        assert_equal 0.5, cred.early_refresh_fraction
        assert_equal 120, cred.early_refresh_slack_seconds
        assert_equal @operator, cred.created_by
        assert_not_includes response.body, "shhh"
        assert_not_includes response.body, "seed-token"
        assert_not_includes response.body, "Basic abc"
      end

      test "create accepts refresh-policy numbers as form strings" do
        create_credential(foreign_id: "string-policy", token_endpoint: "https://idp.example/token", client_id: "cid",
                          early_refresh_fraction: "0.25", early_refresh_slack_seconds: "60",
                          max_refresh_interval_seconds: "600", refresh_timeout_seconds: "5")
        assert_response :created
        cred = BrokerCredential.find_by!(foreign_id: "string-policy")
        assert_equal 0.25, cred.early_refresh_fraction
        assert_equal 5, cred.refresh_timeout_seconds
      end

      test "create without a token endpoint or client id is rejected without writing" do
        assert_no_difference "BrokerCredential.count" do
          create_credential(foreign_id: "broken", client_id: "")
        end
        assert_response :unprocessable_entity
        details = json_body.dig("error", "details")
        assert details.key?("token_endpoint")
        assert details.key?("client_id")
      end

      test "update changes attributes and replaces scopes, headers, and labels" do
        cred = broker_credentials(:acme_managed_gmail)
        update_credential(cred, name: "renamed", client_id: "new-cid", scopes: [ "only.scope" ],
                                token_endpoint_headers: { "X-Tenant" => "acme" }, labels: { "env" => "prod" })
        assert_response :ok
        cred.reload
        assert_equal "renamed", cred.name
        assert_equal "new-cid", cred.client_id
        assert_equal [ "only.scope" ], cred.scopes
        assert_equal({ "X-Tenant" => "acme" }, cred.token_endpoint_headers)
        assert_equal({ "env" => "prod" }, cred.labels)
      end

      test "update with a null header value keeps that header's stored value" do
        cred = broker_credentials(:acme_managed_gmail)
        cred.update!(token_endpoint_headers: { "Authorization" => "Basic stored", "X-Old" => "gone" })

        update_credential(cred, token_endpoint_headers: { "Authorization" => nil, "X-New" => "fresh" })
        assert_response :ok
        assert_equal({ "Authorization" => "Basic stored", "X-New" => "fresh" }, cred.reload.token_endpoint_headers)
        assert_equal %w[Authorization X-New], json_body.dig("data", "token_endpoint_header_names")
        assert_not_includes response.body, "Basic stored"
      end

      test "update with a null value for a new header name is rejected" do
        cred = broker_credentials(:acme_managed_gmail)
        cred.update!(token_endpoint_headers: { "Authorization" => "Basic stored" })

        update_credential(cred, token_endpoint_headers: { "X-Renamed" => nil })
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details").key?("token_endpoint_headers")
        assert_equal({ "Authorization" => "Basic stored" }, cred.reload.token_endpoint_headers)
      end

      test "update with null headers clears them" do
        cred = broker_credentials(:acme_managed_gmail)
        cred.update!(token_endpoint_headers: { "Authorization" => "Basic stored" })
        update_credential(cred, token_endpoint_headers: nil)
        assert_response :ok
        assert_nil cred.reload.token_endpoint_headers
      end

      test "update without headers leaves them in place" do
        cred = broker_credentials(:acme_managed_gmail)
        cred.update!(token_endpoint_headers: { "Authorization" => "Basic stored" })
        update_credential(cred, name: "renamed")
        assert_response :ok
        assert_equal({ "Authorization" => "Basic stored" }, cred.reload.token_endpoint_headers)
      end

      test "update with blank client_secret and refresh_token leaves them in place" do
        cred = broker_credentials(:acme_managed_gmail)
        cred.update!(client_secret: "original-secret", refresh_token: "original-token", dead: true, dead_reason: "stale")

        update_credential(cred, POLICY.merge(client_secret: "", refresh_token: ""))
        assert_response :ok
        cred.reload
        assert_equal "original-secret", cred.client_secret
        assert_equal "original-token", cred.refresh_token
        # A blank seed must not reschedule: dead state is untouched.
        assert cred.dead?
      end

      test "update with blank preqin fields leaves them in place" do
        cred = BrokerCredential.create!(foreign_id: "preqin-console",
                                        grant: "preqin", username: "user", api_key: "api-key",
                                        created_by: @operator)

        update_credential(cred, grant: "preqin", username: "", api_key: "")
        assert_response :ok
        cred.reload
        assert_equal "user", cred.username
        assert_equal "api-key", cred.api_key
      end

      test "update of a preqin username stores it and reschedules the credential" do
        cred = BrokerCredential.create!(foreign_id: "preqin-username",
                                        grant: "preqin", username: "old-user", api_key: "api-key",
                                        dead: true, dead_reason: "http_400", created_by: @operator)

        update_credential(cred, grant: "preqin", username: "preqin-panel-user", api_key: "")
        assert_response :ok
        cred.reload
        assert_equal "preqin-panel-user", cred.username
        assert_equal "api-key", cred.api_key
        assert_not cred.dead?
      end
    end
  end
end
