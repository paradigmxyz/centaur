require "test_helper"

module Api
  module V1
    class PgDsnSecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_pg_dsn_secret_url(id: "pgs_unknown")
        assert_response :unauthorized
      end

      test "GET returns a pg_dsn secret with its dsn source" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        get api_v1_pg_dsn_secret_url(id: secret.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal secret.oid, data["id"]
        assert_equal "analytics", data["database"]
        assert_equal "readonly", data["role"]
        assert_equal({ "source_type" => "env", "config" => { "var" => "PG_ANALYTICS_DSN" } }, data["dsn"])
        # Listener/client config is a proxy-host concern and is not modeled here.
        refute data.key?("listen")
        refute data.key?("client_user")
      end

      test "GET lookup finds a pg_dsn secret by namespace and foreign_id" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        get lookup_api_v1_pg_dsn_secrets_url(namespace: secret.namespace, foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
      end

      test "GET lookup scopes a pg_dsn secret by namespace" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        get lookup_api_v1_pg_dsn_secrets_url(namespace: "globex", foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup returns 404 when no pg_dsn secret matches" do
        get lookup_api_v1_pg_dsn_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a pg_dsn secret with a nested dsn source" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "new-pg",
            name: "orders",
            database: "orders",
            role: "app",
            dsn: { source_type: "aws_sm", config: { secret_id: "arn:db-dsn" } }
          }
        }

        assert_difference -> { PgDsnSecret.count } => 1 do
          post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        secret = PgDsnSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal "aws_sm", secret.dsn_source.source_type
        assert_equal "orders", secret.database
        assert_equal "app", secret.role
      end

      test "POST never echoes a control_plane dsn secret back" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "inline-pg",
            database: "app",
            dsn: { source_type: "control_plane", secret: "postgres://u:sup3rsecret@db/app" }
          }
        }

        post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created
        refute_includes response.body, "sup3rsecret"
      end

      test "POST is rejected when the inline DSN database does not match" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "mismatch-pg",
            database: "app",
            dsn: { source_type: "control_plane", secret: "postgres://u:pw@db/other" }
          }
        }

        assert_no_difference -> { PgDsnSecret.count } do
          post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "POST without a dsn source is rejected" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "no-dsn",
            database: "no-dsn-db"
          }
        }

        assert_no_difference -> { PgDsnSecret.count } do
          post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "POST without a database is rejected" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "no-db",
            dsn: { source_type: "env", config: { var: "PG_DSN" } }
          }
        }

        assert_no_difference -> { PgDsnSecret.count } do
          post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT replaces the dsn source and role" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        body = {
          data: {
            database: secret.database,
            role: "writer",
            dsn: { source_type: "env", config: { var: "ROTATED_DSN" } }
          }
        }

        put api_v1_pg_dsn_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal "ROTATED_DSN", secret.dsn_source.config["var"]
        assert_equal "writer", secret.role
      end

      test "POST persists session settings and echoes them back" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "settings-pg",
            database: "settings-db",
            settings: [
              { name: "app.tenant", value: "centaur" },
              { name: "app.region", value: "us" }
            ],
            dsn: { source_type: "env", config: { var: "SETTINGS_DSN" } }
          }
        }

        post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created

        assert_equal(
          [
            { "name" => "app.tenant", "value" => "centaur" },
            { "name" => "app.region", "value" => "us" }
          ],
          json_body.dig("data", "settings")
        )
        secret = PgDsnSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal "app.tenant", secret.settings.first["name"]
      end

      test "POST persists value_from settings and echoes the stored reference" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "value-from-pg",
            database: "value-from-db",
            settings: [
              { name: "centaur.slack_channel_id", value_from: { principal_label: "slack_channel_id" } },
              { name: "centaur.principal", value_from: { principal_field: "foreign_id" } }
            ],
            dsn: { source_type: "env", config: { var: "VALUE_FROM_DSN" } }
          }
        }

        post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created

        assert_equal(
          [
            { "name" => "centaur.slack_channel_id", "value_from" => { "principal_label" => "slack_channel_id" } },
            { "name" => "centaur.principal", "value_from" => { "principal_field" => "foreign_id" } }
          ],
          json_body.dig("data", "settings")
        )
      end

      test "POST with an invalid value_from is rejected" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "bad-value-from-pg",
            database: "bad-value-from-db",
            settings: [ { name: "app.tenant", value_from: { principal_field: "labels" } } ],
            dsn: { source_type: "env", config: { var: "BAD_VALUE_FROM_DSN" } }
          }
        }

        assert_no_difference -> { PgDsnSecret.count } do
          post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "POST with an invalid setting name is rejected" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "bad-settings-pg",
            database: "bad-settings-db",
            settings: [ { name: "role", value: "x" } ],
            dsn: { source_type: "env", config: { var: "BAD_DSN" } }
          }
        }

        assert_no_difference -> { PgDsnSecret.count } do
          post api_v1_pg_dsn_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT replaces settings as a whole" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        secret.update!(settings: [ { "name" => "app.old", "value" => "1" } ])
        body = {
          data: {
            database: secret.database,
            settings: [ { name: "app.new", value: "2" } ],
            dsn: { source_type: "env", config: { var: "PG_ANALYTICS_DSN" } }
          }
        }

        put api_v1_pg_dsn_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal [ { "name" => "app.new", "value" => "2" } ], secret.settings
      end

      test "PUT resets settings and role when omitted from the body" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        secret.update!(settings: [ { "name" => "app.old", "value" => "1" } ])
        body = {
          data: {
            database: secret.database,
            dsn: { source_type: "env", config: { var: "PG_ANALYTICS_DSN" } }
          }
        }

        put api_v1_pg_dsn_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal [], secret.settings
        assert_nil secret.role
      end

      test "PUT upserts a new pg_dsn secret by foreign_id" do
        body = {
          data: {
            namespace: "acme",
            database: "upsert-db",
            dsn: { source_type: "env", config: { var: "UPSERT_DSN" } }
          }
        }

        assert_difference -> { PgDsnSecret.count } => 1 do
          put api_v1_pg_dsn_secret_url(id: "pg-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal "pg-upsert", json_body.dig("data", "foreign_id")
      end

      test "PUT rejects a pg_dsn secret when another secret uses the same database" do
        existing = pg_dsn_secrets(:acme_analytics_pg)
        body = {
          data: {
            namespace: "acme",
            database: existing.database,
            role: "centaur_readonly",
            dsn: { source_type: "env", config: { var: "SHARED_DATABASE_DSN" } }
          }
        }

        assert_no_difference -> { PgDsnSecret.count } do
          put api_v1_pg_dsn_secret_url(id: "pg-shared-database"),
              params: body.to_json,
              headers: auth_headers
        end
        assert_response :unprocessable_entity
        assert_includes json_body.dig("error", "details", "database"), "has already been taken"
      end

      test "GET index is scoped by namespace" do
        get api_v1_pg_dsn_secrets_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok
        ids = json_body.fetch("data").map { |r| r["id"] }
        assert_includes ids, pg_dsn_secrets(:acme_analytics_pg).oid
      end

      test "DELETE removes a pg_dsn secret and its grants without deleting grantees" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        grant = grants(:acme_channel_analytics_pg)

        assert_difference -> { PgDsnSecret.count } => -1, -> { Grant.count } => -1 do
          delete api_v1_pg_dsn_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil PgDsnSecret.find_by_oid(secret.oid)
        refute Grant.exists?(grant.id)
        assert Principal.exists?(principals(:acme_channel).id)
      end

      test "DELETE returns 404 for an unknown pg_dsn secret" do
        delete api_v1_pg_dsn_secret_url(id: "pgs_nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
