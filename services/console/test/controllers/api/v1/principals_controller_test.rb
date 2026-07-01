require "test_helper"

module Api
  module V1
    class PrincipalsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_principal_url(id: "prn_unknown")
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "rejects requests with an unknown bearer token" do
        get api_v1_principal_url(id: "prn_unknown"),
            headers: auth_headers("iak_not-a-real-token")
        assert_response :unauthorized
      end

      test "rejects requests with a malformed Authorization scheme" do
        get api_v1_principal_url(id: "prn_unknown"),
            headers: { "Authorization" => "Token #{ACME_TOKEN}" }
        assert_response :unauthorized
      end

      test "GET returns a Principal with its labels" do
        principal = principals(:acme_channel)

        get api_v1_principal_url(id: principal.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal principal.oid, data["id"]
        assert_equal "acme", data["namespace"]
        assert_equal "C0123456789", data["foreign_id"]
        assert_equal({ "kind" => "slack_channel", "team" => "platform" }, data["labels"])
        assert_equal true, data["sandbox_repo_cache_enabled"]
        assert_equal true, data["sandbox_observability_enabled"]
      end

      test "GET returns 404 for an unknown oid" do
        get api_v1_principal_url(id: "prn_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET does not resolve a foreign_id passed as :id" do
        principal = principals(:acme_channel)

        get api_v1_principal_url(id: principal.foreign_id), headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a Principal" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "U-new-id",
            labels: { "kind" => "user", "team" => "platform" }
          }
        }

        assert_difference -> { Principal.count } => 1 do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Aprn_/, data["id"])
        assert_equal "acme", data["namespace"]
        assert_equal "U-new-id", data["foreign_id"]
        assert_equal({ "kind" => "user", "team" => "platform" }, data["labels"])
        assert_equal true, data["sandbox_repo_cache_enabled"]
        assert_equal true, data["sandbox_observability_enabled"]
      end

      test "POST creates a Principal with only a human-readable name" do
        body = { data: { name: "Just a label" } }

        assert_difference -> { Principal.count } => 1 do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "Just a label", data["name"]
        assert_equal "default", data["namespace"]
        assert_nil data["foreign_id"]
      end

      test "PUT updates the human-readable name" do
        principal = principals(:acme_channel)
        principal.update!(
          sandbox_repo_cache_enabled: false,
          sandbox_observability_enabled: false
        )
        body = { data: { name: "Acme Slack channel" } }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal "Acme Slack channel", principal.name
        assert_equal false, principal.sandbox_repo_cache_enabled
        assert_equal false, principal.sandbox_observability_enabled
      end

      test "PUT updates sandbox access flags" do
        principal = principals(:acme_channel)
        body = {
          data: {
            sandbox_repo_cache_enabled: false,
            sandbox_observability_enabled: false
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal false, principal.sandbox_repo_cache_enabled
        assert_equal false, principal.sandbox_observability_enabled

        data = json_body.fetch("data")
        assert_equal false, data["sandbox_repo_cache_enabled"]
        assert_equal false, data["sandbox_observability_enabled"]
      end

      test "POST returns 422 when (namespace, foreign_id) already exists" do
        existing = principals(:acme_channel)
        body = {
          data: { namespace: existing.namespace, foreign_id: existing.foreign_id }
        }

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST returns 400 when the data key is missing" do
        post api_v1_principals_url, params: { namespace: "acme" }.to_json, headers: auth_headers
        assert_response :bad_request
      end

      test "PUT updates labels" do
        principal = principals(:acme_channel)
        body = {
          data: { labels: { "kind" => "slack_channel", "team" => "ops" } }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal({ "kind" => "slack_channel", "team" => "ops" }, principal.labels)
      end

      test "PUT ignores attempts to change immutable namespace and foreign_id" do
        principal = principals(:acme_channel)
        original_namespace = principal.namespace
        original_foreign_id = principal.foreign_id

        body = {
          data: {
            namespace: "different-namespace",
            foreign_id: "different-foreign-id",
            labels: { "kind" => "slack_channel" }
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal original_namespace, principal.namespace
        assert_equal original_foreign_id, principal.foreign_id
      end

      test "PUT returns 404 for an unknown oid" do
        put api_v1_principal_url(id: "prn_nope"),
            params: { data: { labels: {} } }.to_json,
            headers: auth_headers
        assert_response :not_found
      end

      test "PUT upserts a new principal by foreign_id" do
        body = { data: { namespace: "acme", name: "Upserted" } }
        assert_difference -> { Principal.count } => 1 do
          put api_v1_principal_url(id: "U-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "acme", data["namespace"]
        assert_equal "U-upsert", data["foreign_id"]
        assert_equal "Upserted", data["name"]
      end

      test "PUT by foreign_id updates an existing principal without creating" do
        principal = principals(:acme_channel)
        body = { data: { namespace: "acme", name: "Renamed channel" } }
        assert_no_difference -> { Principal.count } do
          put api_v1_principal_url(id: principal.foreign_id), params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal "Renamed channel", principal.reload.name
      end

      test "GET index rejects requests without an Authorization header" do
        get api_v1_principals_url, params: { namespace: "acme" }
        assert_response :unauthorized
      end

      test "GET index returns 400 when namespace is missing" do
        get api_v1_principals_url, headers: auth_headers
        assert_response :bad_request
      end

      test "GET index returns all principals in a namespace" do
        get api_v1_principals_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok

        body = json_body
        ids = body.fetch("data").map { |p| p["id"] }
        expected = Principal.where(namespace: "acme").pluck(:id).map { |id| Principal.find(id).oid }
        assert_equal expected.sort, ids.sort
        assert body["data"].all? { |p| p["namespace"] == "acme" }
        assert_equal expected.length, body.dig("meta", "total")
      end

      test "GET index filters by a single label" do
        get api_v1_principals_url,
            params: { namespace: "acme", labels: { kind: "user" } },
            headers: auth_headers
        assert_response :ok

        foreign_ids = json_body.fetch("data").map { |p| p["foreign_id"] }
        assert_equal %w[U-alice U-bob].sort, foreign_ids.sort
      end

      test "GET index ANDs multiple label filters" do
        get api_v1_principals_url,
            params: { namespace: "acme", labels: { kind: "user", team: "platform" } },
            headers: auth_headers
        assert_response :ok

        foreign_ids = json_body.fetch("data").map { |p| p["foreign_id"] }
        assert_equal %w[U-alice], foreign_ids
      end

      test "GET index does not leak across namespaces" do
        get api_v1_principals_url,
            params: { namespace: "acme", labels: { kind: "user", team: "platform" } },
            headers: auth_headers
        assert_response :ok

        assert json_body.fetch("data").none? { |p| p["namespace"] == "globex" }
      end

      test "GET index returns an empty array when no labels match" do
        get api_v1_principals_url,
            params: { namespace: "acme", labels: { kind: "nonexistent" } },
            headers: auth_headers
        assert_response :ok
        assert_equal [], json_body.fetch("data")
        assert_equal 0, json_body.dig("meta", "total")
      end

      test "GET index honors limit and page" do
        get api_v1_principals_url,
            params: { namespace: "acme", limit: 1, page: 2 },
            headers: auth_headers
        assert_response :ok

        body = json_body
        total = Principal.where(namespace: "acme").count
        assert_equal 1, body.fetch("data").length
        assert_equal 1, body.dig("meta", "limit")
        assert_equal 2, body.dig("meta", "page")
        assert_equal total, body.dig("meta", "total")
        assert_equal total, body.dig("meta", "total_pages")
      end

      test "GET lookup finds a principal by namespace and foreign_id" do
        principal = principals(:acme_channel)

        get lookup_api_v1_principals_url(namespace: principal.namespace, foreign_id: principal.foreign_id),
            headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal principal.oid, data["id"]
        assert_equal principal.namespace, data["namespace"]
        assert_equal principal.foreign_id, data["foreign_id"]
      end

      test "GET lookup returns 404 when no principal matches" do
        get lookup_api_v1_principals_url(namespace: "acme", foreign_id: "U-does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup rejects unauthenticated requests" do
        get lookup_api_v1_principals_url(namespace: "acme", foreign_id: "U-alice")
        assert_response :unauthorized
      end

      test "GET lookup scopes by namespace" do
        # globex_user_overlap and acme_user_alice both have similar labels but different namespaces
        get lookup_api_v1_principals_url(namespace: "globex", foreign_id: "U-alice"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST rejects a non-URL-safe foreign_id" do
        body = { data: { namespace: "acme", foreign_id: "bad/value" } }
        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST rejects a non-URL-safe namespace" do
        body = { data: { namespace: "acme corp", foreign_id: "U-ok" } }
        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "GET index clamps limit above the max" do
        get api_v1_principals_url,
            params: { namespace: "acme", limit: 9999 },
            headers: auth_headers
        assert_response :ok
        assert_equal 200, json_body.dig("meta", "limit")
      end

      # acme_channel is granted github_token_inject and db_password_replace (see
      # grants.yml); give them sources so they materialize into the config.
      def grant_sources_to_acme_channel
        SecretSource.create!(source_type: "env", config: { "var" => "GITHUB_TOKEN" },
                             static_secret: static_secrets(:github_token_inject))
        SecretSource.create!(source_type: "control_plane", secret: "s3cr3t-db-pass",
                             static_secret: static_secrets(:db_password_replace))
      end

      test "GET effective_config returns the principal's resolved config" do
        grant_sources_to_acme_channel
        principal = principals(:acme_channel)

        get effective_config_api_v1_principal_url(id: principal.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal principal.oid, data["id"]
        assert_equal 2, data.fetch("secrets").length
        assert_kind_of Array, data.fetch("transforms")
        assert_kind_of Array, data.fetch("postgres")
      end

      test "GET effective_config redacts inline control_plane secret values" do
        grant_sources_to_acme_channel

        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid), headers: auth_headers
        assert_response :ok

        entry = json_body.dig("data", "secrets").find { |s| s.dig("source", "type") == "control_plane" }
        refute_nil entry
        assert_equal "[redacted]", entry.dig("source", "value")
        # A reference-style source passes through unredacted.
        env = json_body.dig("data", "secrets").find { |s| s.dig("source", "type") == "env" }
        assert_equal "GITHUB_TOKEN", env.dig("source", "var")
      end

      test "GET effective_config omits the config_hash" do
        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid), headers: auth_headers
        assert_response :ok
        refute json_body.fetch("data").key?("config_hash")
      end

      test "GET effective_config sends an ETag and forbids caching" do
        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid), headers: auth_headers
        assert_response :ok
        assert_match(/\A"[0-9a-f]{64}"\z/, response.headers["ETag"])
        assert_equal "no-store", response.headers["Cache-Control"]
      end

      test "GET effective_config resolves a namespaced foreign_id via the lookup route" do
        grant_sources_to_acme_channel
        principal = principals(:acme_channel)

        get lookup_effective_config_api_v1_principals_url(namespace: principal.namespace,
                                                          foreign_id: principal.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal principal.oid, json_body.dig("data", "id")
        assert_equal 2, json_body.dig("data", "secrets").length
      end

      test "GET effective_config lookup scopes a foreign_id by namespace" do
        principal = principals(:acme_channel)
        get lookup_effective_config_api_v1_principals_url(namespace: "globex",
                                                          foreign_id: principal.foreign_id),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config does not resolve a foreign_id passed as :id" do
        principal = principals(:acme_channel)
        get effective_config_api_v1_principal_url(id: principal.foreign_id), headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config returns 404 for an unknown oid" do
        get effective_config_api_v1_principal_url(id: "prn_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config lookup returns 404 for an unknown foreign_id" do
        get lookup_effective_config_api_v1_principals_url(namespace: "acme",
                                                          foreign_id: "U-does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config rejects unauthenticated requests" do
        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid)
        assert_response :unauthorized
      end
    end
  end
end
