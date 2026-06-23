require "test_helper"

module Api
  module V1
    class GcpIdTokenSecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_gcp_id_token_secret_url(id: "gid_unknown")
        assert_response :unauthorized
      end

      test "GET returns a gcp_id_token secret with its keyfile and rules" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        get api_v1_gcp_id_token_secret_url(id: secret.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal secret.oid, data["id"]
        assert_equal "https://my-service-abc123-uc.a.run.app", data["audience"]
        assert_equal "x-serverless-authorization", data["header"]
        assert_equal({ "source_type" => "env", "config" => { "var" => "CLOUD_RUN_SA_KEYFILE" } },
                     data["keyfile"])
        assert_equal 1, data["rules"].length
      end

      test "GET lookup finds a gcp_id_token secret by namespace and foreign_id" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        get lookup_api_v1_gcp_id_token_secrets_url(namespace: secret.namespace, foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
      end

      test "GET lookup returns 404 when no gcp_id_token secret matches" do
        get lookup_api_v1_gcp_id_token_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a gcp_id_token secret" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "new-cloud-run",
            name: "new cloud run",
            audience: "https://new-service-abc123-uc.a.run.app",
            header: "X-Serverless-Authorization",
            keyfile: { source_type: "env", config: { var: "NEW_CLOUD_RUN_KEYFILE" } },
            rules: [ { host: "new-service-abc123-uc.a.run.app" } ]
          }
        }

        assert_difference -> { GcpIdTokenSecret.count } => 1,
                          -> { SecretSource.count } => 1,
                          -> { RequestRule.count } => 1 do
          post api_v1_gcp_id_token_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        secret = GcpIdTokenSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal "x-serverless-authorization", secret.header
        assert_equal "NEW_CLOUD_RUN_KEYFILE", secret.keyfile_source.config["var"]
      end

      test "POST never echoes a control_plane keyfile secret back" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "inline-cloud-run",
            audience: "https://inline-service-abc123-uc.a.run.app",
            keyfile: { source_type: "control_plane", secret: "{\"client_email\":\"x\"}" },
            rules: [ { host: "inline-service-abc123-uc.a.run.app" } ]
          }
        }

        post api_v1_gcp_id_token_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created
        refute_includes response.body, "client_email"
      end

      test "POST rejects missing rules" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "no-rules",
            audience: "https://no-rules-abc123-uc.a.run.app",
            keyfile: { source_type: "env", config: { var: "CLOUD_RUN_KEYFILE" } }
          }
        }

        assert_no_difference -> { GcpIdTokenSecret.count } do
          post api_v1_gcp_id_token_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT replaces rules and keyfile" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        body = {
          data: {
            audience: "https://rotated-service-abc123-uc.a.run.app",
            header: "",
            keyfile: { source_type: "env", config: { var: "ROTATED_CLOUD_RUN_KEYFILE" } },
            rules: [ { host: "rotated-service-abc123-uc.a.run.app", http_methods: [ "POST" ] } ]
          }
        }

        put api_v1_gcp_id_token_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal "https://rotated-service-abc123-uc.a.run.app", secret.audience
        assert_nil secret.header
        assert_equal "ROTATED_CLOUD_RUN_KEYFILE", secret.keyfile_source.config["var"]
        assert_equal [ "rotated-service-abc123-uc.a.run.app" ], secret.rules.map(&:host)
      end

      test "PUT upserts a new gcp_id_token secret by foreign_id" do
        body = {
          data: {
            namespace: "acme",
            audience: "https://upsert-service-abc123-uc.a.run.app",
            keyfile: { source_type: "env", config: { var: "UPSERT_CLOUD_RUN_KEYFILE" } },
            rules: [ { host: "upsert-service-abc123-uc.a.run.app" } ]
          }
        }

        assert_difference -> { GcpIdTokenSecret.count } => 1 do
          put api_v1_gcp_id_token_secret_url(id: "cloud-run-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal "cloud-run-upsert", json_body.dig("data", "foreign_id")
      end

      test "GET index is scoped by namespace" do
        get api_v1_gcp_id_token_secrets_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok
        ids = json_body.fetch("data").map { |r| r["id"] }
        assert_includes ids, gcp_id_token_secrets(:acme_cloud_run).oid
      end

      test "DELETE removes a gcp_id_token secret" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        assert_difference -> { GcpIdTokenSecret.count } => -1 do
          delete api_v1_gcp_id_token_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil GcpIdTokenSecret.find_by_oid(secret.oid)
      end
    end
  end
end
