require "test_helper"

module Console
  # Covers the per-type secret form controllers (StaticSecrets, PgDsnSecrets) and
  # their shared base flow: what gets built/persisted, redirects, and that invalid
  # input is rejected (422) without writing. Rendered markup is intentionally not
  # asserted here.
  class SecretsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    # --- routing / gating -------------------------------------------------

    test "redirects to login when not signed in" do
      delete logout_url
      get new_console_static_secret_url
      assert_redirected_to login_path
    end

    test "a kind without a form has no route and 404s" do
      get "/console/secrets/hmac/new"
      assert_response :not_found
    end

    # --- static -----------------------------------------------------------

    test "GET new and edit render without error" do
      get new_console_static_secret_url
      assert_response :ok
      get edit_console_static_secret_url(static_secrets(:acme_prod_api_key).oid)
      assert_response :ok
    end

    # The managed-secret guard banner is behavior (a warning), not form markup, so
    # it is asserted here unlike the rest of the rendered form.
    test "edit warns when the secret is an OAuth-flow-managed wrapper" do
      get edit_console_static_secret_url(static_secrets(:acme_managed_gmail_secret).oid)
      assert_response :ok
      assert_match "Managed secret", response.body
      # An ordinary secret shows no such warning.
      get edit_console_static_secret_url(static_secrets(:acme_prod_api_key).oid)
      assert_no_match "Managed secret", response.body
    end

    test "POST create builds a static secret with a source and rules" do
      assert_difference -> { StaticSecret.count } => 1,
                        -> { SecretSource.count } => 1,
                        -> { RequestRule.count } => 2 do
        post console_static_secrets_url, params: {
          secret: { namespace: "acme", name: "ui-static", foreign_id: "ui-static" },
          static: { mode: "inject", header: "Authorization", formatter: "Bearer {{ .Value }}" },
          source: { source_type: "env", reference: "UI_TOKEN" },
          rules: {
            "0" => { host: "api.example.com", http_methods: "get, post", paths: "/v1/*" },
            "1" => { host: "api2.example.com", http_methods: "POST", paths: "" }
          },
          labels: { "0" => { key: "team", value: "platform" } }
        }
      end

      secret = StaticSecret.find_by!(namespace: "acme", foreign_id: "ui-static")
      assert_redirected_to console_secret_path("static", secret.oid)
      assert_equal({ "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }, secret.inject_config)
      assert_equal({ "team" => "platform" }, secret.labels)
      assert_equal({ "var" => "UI_TOKEN" }, secret.source.config)
      assert_equal [ 0, 1 ], secret.rules.order(:position).map(&:position)
      assert_equal %w[GET POST], secret.rules.order(:position).first.http_methods
    end

    test "POST create with no inject or replace is rejected without writing" do
      assert_no_difference [ "StaticSecret.count", "SecretSource.count", "RequestRule.count" ] do
        post console_static_secrets_url, params: {
          secret: { namespace: "acme", name: "broken" },
          static: { mode: "inject" },
          source: { source_type: "env", reference: "X" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "POST create with an invalid nested rule is rejected without writing" do
      assert_no_difference [ "StaticSecret.count", "RequestRule.count" ] do
        post console_static_secrets_url, params: {
          secret: { namespace: "acme", name: "bad-rule" },
          static: { mode: "inject", header: "Authorization" },
          rules: { "0" => { host: "h.example.com", cidr: "10.0.0.0/8" } }
        }
      end
      assert_response :unprocessable_entity
    end

    test "PATCH update changes attributes and replaces rules" do
      secret = static_secrets(:github_token_inject)
      patch console_static_secret_url(secret.oid), params: {
        secret: { namespace: secret.namespace, name: "renamed" },
        static: { mode: "inject", header: "X-Token" },
        source: { source_type: "env", reference: "NEW_VAR" },
        rules: { "0" => { host: "only.example.com", http_methods: "GET", paths: "/" } }
      }
      assert_redirected_to console_secret_path("static", secret.oid)
      secret.reload
      assert_equal "renamed", secret.name
      assert_equal({ "header" => "X-Token" }, secret.inject_config)
      assert_equal "NEW_VAR", secret.source.config["var"]
      assert_equal [ "only.example.com" ], secret.rules.map(&:host)
    end

    test "DELETE destroy removes the secret and cascades its grants" do
      secret = static_secrets(:github_token_inject) # granted directly to acme_channel
      assert_difference -> { StaticSecret.count } => -1, -> { Grant.count } => -1 do
        delete console_static_secret_url(secret.oid)
      end
      assert_redirected_to console_secrets_path
      assert_equal "Secret deleted.", flash[:notice]
      assert_not StaticSecret.exists?(secret.id)
    end

    # --- pg_dsn -----------------------------------------------------------

    test "GET new renders without error" do
      get new_console_pg_dsn_secret_url
      assert_response :ok
    end

    test "POST create builds a pg_dsn secret with an inline DSN source" do
      assert_difference -> { PgDsnSecret.count } => 1, -> { SecretSource.count } => 1 do
        post console_pg_dsn_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-shop", database: "shop", role: "readonly" },
          source: { source_type: "control_plane", secret: "postgres://u:p@db.example:5432/shop" }
        }
      end
      secret = PgDsnSecret.find_by!(namespace: "acme", foreign_id: "ui-shop")
      assert_redirected_to console_secret_path("pg_dsn", secret.oid)
      assert_equal "shop", secret.database
      assert_equal "control_plane", secret.dsn_source.source_type
    end

    test "POST create rejects a database that mismatches the inline DSN" do
      assert_no_difference [ "PgDsnSecret.count", "SecretSource.count" ] do
        post console_pg_dsn_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-mismatch", database: "wrong" },
          source: { source_type: "control_plane", secret: "postgres://u:p@db.example/shop" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "POST create requires foreign_id and database for pg_dsn" do
      assert_no_difference "PgDsnSecret.count" do
        post console_pg_dsn_secrets_url, params: {
          secret: { namespace: "acme" },
          source: { source_type: "env", reference: "PG_DSN" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "PATCH update changes the pg_dsn database and source" do
      secret = pg_dsn_secrets(:acme_reporting_pg)
      patch console_pg_dsn_secret_url(secret.oid), params: {
        secret: { namespace: secret.namespace, foreign_id: secret.foreign_id, database: "reporting", role: "" },
        source: { source_type: "env", reference: "REPORTING_DSN" }
      }
      assert_redirected_to console_secret_path("pg_dsn", secret.oid)
      secret.reload
      assert_nil secret.role.presence
      assert_equal "REPORTING_DSN", secret.dsn_source.config["var"]
    end

    test "DELETE destroy removes a pg_dsn secret" do
      secret = pg_dsn_secrets(:acme_reporting_pg)
      assert_difference -> { PgDsnSecret.count } => -1 do
        delete console_pg_dsn_secret_url(secret.oid)
      end
      assert_redirected_to console_secrets_path
      assert_equal "Secret deleted.", flash[:notice]
    end

    test "POST create captures ordered session settings and drops blank-name rows" do
      post console_pg_dsn_secrets_url, params: {
        secret: { namespace: "acme", foreign_id: "ui-settings", database: "settingsdb" },
        settings: {
          "0" => { name: "app.tenant", value: "centaur" },
          "1" => { name: "", value: "ignored" },
          "2" => { name: "app.region", value: "us" }
        },
        source: { source_type: "env", reference: "SETTINGS_DSN" }
      }
      secret = PgDsnSecret.find_by!(namespace: "acme", foreign_id: "ui-settings")
      assert_redirected_to console_secret_path("pg_dsn", secret.oid)
      assert_equal(
        [
          { "name" => "app.tenant", "value" => "centaur" },
          { "name" => "app.region", "value" => "us" }
        ],
        secret.settings
      )
    end

    test "POST create captures principal-derived settings via the kind select" do
      post console_pg_dsn_secrets_url, params: {
        secret: { namespace: "acme", foreign_id: "ui-value-from", database: "valuefromdb" },
        settings: {
          "0" => { name: "centaur.slack_channel_id", kind: "principal_label", value: "slack_channel_id" },
          "1" => { name: "centaur.principal", kind: "principal_field", value: "foreign_id" },
          "2" => { name: "app.tenant", kind: "literal", value: "centaur" }
        },
        source: { source_type: "env", reference: "VALUE_FROM_DSN" }
      }
      secret = PgDsnSecret.find_by!(namespace: "acme", foreign_id: "ui-value-from")
      assert_redirected_to console_secret_path("pg_dsn", secret.oid)
      assert_equal(
        [
          { "name" => "centaur.slack_channel_id", "value_from" => { "principal_label" => "slack_channel_id" } },
          { "name" => "centaur.principal", "value_from" => { "principal_field" => "foreign_id" } },
          { "name" => "app.tenant", "value" => "centaur" }
        ],
        secret.settings
      )
    end

    test "POST create rejects an unknown principal_field from the console form" do
      assert_no_difference "PgDsnSecret.count" do
        post console_pg_dsn_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-bad-field", database: "badfielddb" },
          settings: { "0" => { name: "app.tenant", kind: "principal_field", value: "labels" } },
          source: { source_type: "env", reference: "SETTINGS_DSN" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "PATCH update with no settings rows clears them" do
      secret = pg_dsn_secrets(:acme_reporting_pg)
      secret.update!(settings: [ { "name" => "app.tenant", "value" => "centaur" } ])
      patch console_pg_dsn_secret_url(secret.oid), params: {
        secret: { namespace: secret.namespace, foreign_id: secret.foreign_id, database: "reporting" },
        source: { source_type: "env", reference: "REPORTING_DSN" }
      }
      assert_redirected_to console_secret_path("pg_dsn", secret.oid)
      assert_equal [], secret.reload.settings
    end

    test "POST create rejects an invalid session setting name" do
      assert_no_difference "PgDsnSecret.count" do
        post console_pg_dsn_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-bad-setting", database: "badsettingdb" },
          settings: { "0" => { name: "session_authorization", value: "x" } },
          source: { source_type: "env", reference: "SETTINGS_DSN" }
        }
      end
      assert_response :unprocessable_entity
    end

    # --- gcp_auth ---------------------------------------------------------

    test "GET new and edit render without error for gcp_auth" do
      get new_console_gcp_auth_secret_url
      assert_response :ok
      get edit_console_gcp_auth_secret_url(gcp_auth_secrets(:acme_gcs_keyfile).oid)
      assert_response :ok
    end

    test "POST create builds a keyfile gcp_auth secret with a source and rules" do
      assert_difference -> { GcpAuthSecret.count } => 1,
                        -> { SecretSource.count } => 1,
                        -> { RequestRule.count } => 1 do
        post console_gcp_auth_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-gcp-key", name: "ui-gcp" },
          gcp: {
            credential_mode: "keyfile",
            subject: "bot@acme.example",
            scopes: "https://www.googleapis.com/auth/cloud-platform\nhttps://www.googleapis.com/auth/devstorage.read_only"
          },
          source: { source_type: "env", reference: "GCP_KEY" },
          rules: { "0" => { host: "storage.googleapis.com", http_methods: "GET", paths: "/" } }
        }
      end

      secret = GcpAuthSecret.find_by!(namespace: "acme", foreign_id: "ui-gcp-key")
      assert_redirected_to console_secret_path("gcp_auth", secret.oid)
      assert_equal %w[https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/devstorage.read_only], secret.scopes
      assert_equal "bot@acme.example", secret.subject
      assert_nil secret.credentials_provider
      assert_equal({ "var" => "GCP_KEY" }, secret.keyfile_source.config)
      assert_equal [ "storage.googleapis.com" ], secret.rules.map(&:host)
    end

    test "POST create builds a workload-identity gcp_auth secret and ignores the source" do
      assert_difference -> { GcpAuthSecret.count } => 1 do
        assert_no_difference "SecretSource.count" do
          post console_gcp_auth_secrets_url, params: {
            secret: { namespace: "acme", foreign_id: "ui-gcp-wi" },
            gcp: {
              credential_mode: "workload_identity",
              subject: "ignored@acme.example",
              scopes: "https://www.googleapis.com/auth/cloud-platform"
            },
            source: { source_type: "env", reference: "SHOULD_BE_IGNORED" }
          }
        end
      end

      secret = GcpAuthSecret.find_by!(namespace: "acme", foreign_id: "ui-gcp-wi")
      assert_redirected_to console_secret_path("gcp_auth", secret.oid)
      assert_equal({ "type" => "workload_identity" }, secret.credentials_provider)
      assert_nil secret.keyfile_source
      assert_nil secret.subject
    end

    test "POST create gcp_auth without scopes is rejected without writing" do
      assert_no_difference [ "GcpAuthSecret.count", "SecretSource.count" ] do
        post console_gcp_auth_secrets_url, params: {
          secret: { namespace: "acme", name: "no-scopes" },
          gcp: { credential_mode: "keyfile", scopes: "" },
          source: { source_type: "env", reference: "GCP_KEY" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "PATCH update switches a keyfile gcp_auth secret to workload identity" do
      secret = gcp_auth_secrets(:acme_gcs_keyfile)
      assert secret.keyfile_source.present?

      patch console_gcp_auth_secret_url(secret.oid), params: {
        secret: { namespace: secret.namespace, foreign_id: secret.foreign_id },
        gcp: { credential_mode: "workload_identity", scopes: "https://www.googleapis.com/auth/cloud-platform" }
      }

      assert_redirected_to console_secret_path("gcp_auth", secret.oid)
      secret.reload
      assert_nil secret.keyfile_source
      assert_nil secret.subject
      assert_equal({ "type" => "workload_identity" }, secret.credentials_provider)
    end

    # --- gcp_id_token -----------------------------------------------------

    test "GET new and edit render without error for gcp_id_token" do
      get new_console_gcp_id_token_secret_url
      assert_response :ok
      get edit_console_gcp_id_token_secret_url(gcp_id_token_secrets(:acme_cloud_run).oid)
      assert_response :ok
    end

    test "POST create builds a gcp_id_token secret with a keyfile source and rule" do
      assert_difference -> { GcpIdTokenSecret.count } => 1,
                        -> { SecretSource.count } => 1,
                        -> { RequestRule.count } => 1 do
        post console_gcp_id_token_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-cloud-run", name: "ui cloud run" },
          gcp_id_token: {
            audience: "https://ui-service-abc123-uc.a.run.app",
            header: "x-serverless-authorization"
          },
          source: { source_type: "env", reference: "UI_CLOUD_RUN_KEYFILE" },
          rules: { "0" => { host: "ui-service-abc123-uc.a.run.app", http_methods: "GET", paths: "/" } }
        }
      end

      secret = GcpIdTokenSecret.find_by!(namespace: "acme", foreign_id: "ui-cloud-run")
      assert_redirected_to console_secret_path("gcp_id_token", secret.oid)
      assert_equal "https://ui-service-abc123-uc.a.run.app", secret.audience
      assert_equal "x-serverless-authorization", secret.header
      assert_equal({ "var" => "UI_CLOUD_RUN_KEYFILE" }, secret.keyfile_source.config)
      assert_equal [ "ui-service-abc123-uc.a.run.app" ], secret.rules.map(&:host)
    end

    test "POST create gcp_id_token without rules is rejected without writing" do
      assert_no_difference [ "GcpIdTokenSecret.count", "SecretSource.count", "RequestRule.count" ] do
        post console_gcp_id_token_secrets_url, params: {
          secret: { namespace: "acme", foreign_id: "ui-cloud-run-no-rules" },
          gcp_id_token: { audience: "https://ui-service-abc123-uc.a.run.app" },
          source: { source_type: "env", reference: "UI_CLOUD_RUN_KEYFILE" }
        }
      end
      assert_response :unprocessable_entity
      assert_match "Rules must include at least one rule", response.body
      assert_match "must include at least one rule", response.body
    end

    test "PATCH update changes a gcp_id_token secret keyfile audience header and rules" do
      secret = gcp_id_token_secrets(:acme_cloud_run)

      patch console_gcp_id_token_secret_url(secret.oid), params: {
        secret: { namespace: secret.namespace, foreign_id: secret.foreign_id },
        gcp_id_token: {
          audience: "https://updated-service-abc123-uc.a.run.app",
          header: ""
        },
        source: { source_type: "env", reference: "UPDATED_CLOUD_RUN_KEYFILE" },
        rules: { "0" => { host: "updated-service-abc123-uc.a.run.app", http_methods: "POST", paths: "" } }
      }

      assert_redirected_to console_secret_path("gcp_id_token", secret.oid)
      secret.reload
      assert_equal "https://updated-service-abc123-uc.a.run.app", secret.audience
      assert_nil secret.header
      assert_equal "UPDATED_CLOUD_RUN_KEYFILE", secret.keyfile_source.config["var"]
      assert_equal [ "updated-service-abc123-uc.a.run.app" ], secret.rules.map(&:host)
    end

    # --- role grants ------------------------------------------------------

    test "POST grant_role grants the secret to a role at the default role priority" do
      secret = static_secrets(:acme_staging_api_key)
      role = roles(:acme_admin_role)

      assert_difference -> { role.grants.count }, 1 do
        post console_secret_grant_role_url("static", secret.oid), params: { role_id: role.oid }
      end

      assert_redirected_to console_secret_path("static", secret.oid)
      grant = role.grants.find_by(static_secret: secret)
      assert_not_nil grant
      assert_equal Grant::DEFAULT_ROLE_PRIORITY, grant.priority
    end

    test "POST grant_role is idempotent" do
      secret = static_secrets(:acme_prod_api_key)
      role = roles(:acme_infra)

      assert_no_difference -> { role.grants.count } do
        post console_secret_grant_role_url("static", secret.oid), params: { role_id: role.oid }
      end

      assert_redirected_to console_secret_path("static", secret.oid)
    end

    test "POST grant_role rejects a role from another namespace" do
      secret = static_secrets(:acme_staging_api_key)
      role = roles(:globex_infra)

      assert_no_difference -> { Grant.count } do
        post console_secret_grant_role_url("static", secret.oid), params: { role_id: role.oid }
      end

      assert_redirected_to console_secret_path("static", secret.oid)
      assert_equal "Role must be in the same namespace as the secret.", flash[:alert]
    end

    test "DELETE revoke_role_grant removes a role grant from the secret" do
      secret = static_secrets(:acme_prod_api_key)
      grant = grants(:acme_infra_prod_api_key)

      assert_difference -> { roles(:acme_infra).grants.count }, -1 do
        delete console_secret_revoke_role_grant_url("static", secret.oid, grant.oid)
      end

      assert_redirected_to console_secret_path("static", secret.oid)
      assert_not Grant.exists?(grant.id)
    end
  end
end
