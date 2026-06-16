require "test_helper"

module Api
  module V1
    class GcpAuthSecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_gcp_auth_secret_url(id: "gas_unknown")
        assert_response :unauthorized
      end

      test "GET returns a gcp_auth secret with its keyfile and rules" do
        secret = gcp_auth_secrets(:acme_gcs_keyfile)
        get api_v1_gcp_auth_secret_url(id: secret.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal secret.oid, data["id"]
        assert_equal({ "source_type" => "env", "config" => { "var" => "GCP_SA_KEYFILE" } }, data["keyfile"])
        assert_equal "storage-bot@acme.example", data["subject"]
      end

      test "GET lookup finds a gcp_auth secret by namespace and foreign_id" do
        secret = gcp_auth_secrets(:acme_gcs_keyfile)
        get lookup_api_v1_gcp_auth_secrets_url(namespace: secret.namespace, foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
      end

      test "GET lookup scopes a gcp_auth secret by namespace" do
        secret = gcp_auth_secrets(:acme_gcs_keyfile)
        get lookup_api_v1_gcp_auth_secrets_url(namespace: "globex", foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup returns 404 when no gcp_auth secret matches" do
        get lookup_api_v1_gcp_auth_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a gcp_auth secret with credentials_provider" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "new-wif",
            name: "wif",
            credentials_provider: { type: "workload_identity" },
            scopes: [ "https://www.googleapis.com/auth/cloud-platform" ],
            rules: [ { host: "*.googleapis.com" } ]
          }
        }

        assert_difference -> { GcpAuthSecret.count } => 1 do
          post api_v1_gcp_auth_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal({ "type" => "workload_identity" }, data["credentials_provider"])
        assert_equal 1, data["rules"].length
      end

      test "POST creates a gcp_auth secret with a nested keyfile source" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "new-keyfile",
            keyfile: { source_type: "env", config: { var: "MY_SA" } },
            subject: "bot@acme.example",
            scopes: [ "scopeA" ]
          }
        }

        post api_v1_gcp_auth_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created

        secret = GcpAuthSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal "env", secret.keyfile_source.source_type
        assert_equal "bot@acme.example", secret.subject
      end

      test "POST never echoes a control_plane keyfile secret back" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "inline-keyfile",
            keyfile: { source_type: "control_plane", secret: "{\"client_email\":\"x\"}" },
            scopes: [ "scopeA" ]
          }
        }

        post api_v1_gcp_auth_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created
        refute_includes response.body, "client_email"
      end

      test "POST rejects both keyfile and credentials_provider" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "conflict",
            keyfile: { source_type: "env", config: { var: "MY_SA" } },
            credentials_provider: { type: "workload_identity" },
            scopes: [ "scopeA" ]
          }
        }

        assert_no_difference -> { GcpAuthSecret.count } do
          post api_v1_gcp_auth_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT replaces rules and keyfile" do
        secret = gcp_auth_secrets(:acme_gcs_keyfile)
        body = {
          data: {
            keyfile: { source_type: "env", config: { var: "ROTATED_SA" } },
            subject: "storage-bot@acme.example",
            scopes: [ "scopeB" ],
            rules: [ { host: "storage.googleapis.com" } ]
          }
        }

        put api_v1_gcp_auth_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal "ROTATED_SA", secret.keyfile_source.config["var"]
        assert_equal [ "scopeB" ], secret.scopes
        assert_equal 1, secret.rules.count
      end

      test "PUT switches a secret from credentials_provider to keyfile" do
        secret = gcp_auth_secrets(:acme_bigquery)
        body = {
          data: {
            name: secret.name,
            credentials_provider: nil,
            keyfile: { source_type: "env", config: { var: "BQ_SA" } },
            scopes: [ "scopeA" ]
          }
        }

        put api_v1_gcp_auth_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_nil secret.credentials_provider
        assert_equal "BQ_SA", secret.keyfile_source.config["var"]
      end

      test "PUT switches a secret from keyfile to credentials_provider and clears the stale subject" do
        secret = gcp_auth_secrets(:acme_gcs_keyfile)
        body = {
          data: {
            name: secret.name,
            credentials_provider: { type: "workload_identity" },
            scopes: [ "scopeA" ]
          }
        }

        put api_v1_gcp_auth_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal "workload_identity", secret.credentials_provider["type"]
        assert_nil secret.subject
        assert_nil secret.keyfile_source
      end

      test "PUT upserts a new gcp_auth secret by foreign_id" do
        body = {
          data: {
            namespace: "acme",
            credentials_provider: { type: "workload_identity" },
            scopes: [ "scopeA" ],
            rules: [ { host: "*.googleapis.com" } ]
          }
        }

        assert_difference -> { GcpAuthSecret.count } => 1 do
          put api_v1_gcp_auth_secret_url(id: "wif-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal "wif-upsert", json_body.dig("data", "foreign_id")
      end

      test "GET index is scoped by namespace" do
        get api_v1_gcp_auth_secrets_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok
        ids = json_body.fetch("data").map { |r| r["id"] }
        assert_includes ids, gcp_auth_secrets(:acme_bigquery).oid
      end

      test "DELETE removes a gcp_auth secret" do
        secret = gcp_auth_secrets(:acme_bigquery)
        assert_difference -> { GcpAuthSecret.count } => -1 do
          delete api_v1_gcp_auth_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil GcpAuthSecret.find_by_oid(secret.oid)
      end

      test "DELETE returns 404 for an unknown gcp_auth secret" do
        delete api_v1_gcp_auth_secret_url(id: "gas_nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
