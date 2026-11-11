require "test_helper"

module Api
  module V1
    # The /api/v1 surface behind the console secrets pages, ported from the
    # former HTML controller tests (console_controller_test.rb secret cases and
    # console/secrets_controller_test.rb). Requests use the console session
    # cookie (+ CSRF) as the SPA does; bodies are the JSON the SPA forms send.
    class ConsoleSecretsTest < ActionDispatch::IntegrationTest
      setup do
        @operator = users(:acme_admin)
        sign_in_console(@operator)
      end

      # -- authorization -----------------------------------------------------

      test "secret list, detail, and role grant endpoints are admin-only" do
        secret = static_secrets(:acme_prod_api_key)
        grant = grants(:acme_infra_prod_api_key)
        assert_admin_only_api :get, api_v1_secrets_path
        assert_admin_only_api :get, api_v1_secret_detail_path("static", secret.oid)
        assert_admin_only_api :get, api_v1_secret_role_grants_path("static", secret.oid)
        assert_admin_only_api :post, api_v1_secret_role_grants_path("static", secret.oid),
                              params: { data: { role_id: roles(:acme_admin_role).oid } }
        assert_admin_only_api :delete, api_v1_secret_role_grant_path("static", secret.oid, grant.oid)
        assert Grant.exists?(grant.id)
        assert_not Grant.exists?(role: roles(:acme_admin_role), static_secret: secret)
      end

      test "the per-kind writes the forms use are admin-only" do
        {
          "static" => [ api_v1_static_secrets_path, static_secrets(:acme_prod_api_key) ],
          "pg_dsn" => [ api_v1_pg_dsn_secrets_path, pg_dsn_secrets(:acme_reporting_pg) ],
          "gcp_auth" => [ api_v1_gcp_auth_secrets_path, gcp_auth_secrets(:acme_gcs_keyfile) ],
          "gcp_id_token" => [ api_v1_gcp_id_token_secrets_path, gcp_id_token_secrets(:acme_cloud_run) ]
        }.each do |_kind, (collection, record)|
          assert_admin_only_api :get, "#{collection}/#{record.oid}"
          assert_admin_only_api :post, collection, params: { data: { name: "sneaky" } }
          assert_admin_only_api :patch, "#{collection}/#{record.oid}", params: { data: { name: "sneaky" } }
          assert_admin_only_api :delete, "#{collection}/#{record.oid}"
          assert record.class.exists?(record.id)
          assert_not_equal "sneaky", record.reload.name
        end
      end

      test "bearer admins can read the console secret views" do
        get api_v1_secret_detail_path("static", static_secrets(:acme_prod_api_key).oid), headers: bearer_headers
        assert_response :ok
        get api_v1_secret_role_grants_path("static", static_secrets(:acme_prod_api_key).oid), headers: bearer_headers
        assert_response :ok
      end

      # -- list ----------------------------------------------------------------

      test "secrets list combines kinds with backend labels but never references" do
        secret = static_secrets(:acme_prod_api_key)
        get api_v1_secrets_path(limit: 200)
        assert_response :ok

        row = json_body["data"].find { |s| s["id"] == secret.oid }
        assert_equal "static", row["kind"]
        assert_equal "Static", row["kind_label"]
        assert_equal secret.name, row["name"]
        assert_equal secret.created_at.as_json, row["created_at"]
        assert_includes json_body["data"].map { |s| s["kind"] }, "pg_dsn"
        assert_includes json_body["data"].flat_map { |s| s["source_types"] }, "Env"
        # The list shows only the backend label, not the underlying reference.
        assert_no_match(/GITHUB_TOKEN|GCP_SA_KEYFILE|op:\/\//, response.body)
      end

      test "secrets list filters by kind and searches by name" do
        matching = static_secrets(:acme_prod_api_key)
        get api_v1_secrets_path(q: "PROD-API", kind: "static")
        ids = json_body["data"].map { |s| s["id"] }
        assert_includes ids, matching.oid
        assert_not_includes ids, static_secrets(:acme_staging_api_key).oid
        assert_not_includes ids, pg_dsn_secrets(:acme_reporting_pg).oid
      end

      test "secrets list paginates fifty filtered records per page" do
        now = Time.current
        StaticSecret.insert_all!((1..51).map do |number|
          { created_at: now, updated_at: now, name: "Pagination secret #{number}", inject_config: { header: "Authorization" } }
        end)

        get api_v1_secrets_path(q: "Pagination secret", kind: "static")
        assert_equal 50, json_body["data"].size
        assert_equal({ "page" => 1, "limit" => 50, "total" => 51, "total_pages" => 2 }, json_body["meta"])

        get api_v1_secrets_path(q: "Pagination secret", kind: "static", page: 2)
        assert_equal 1, json_body["data"].size
      end

      test "secrets list badges an OAuth-flow-managed static secret" do
        managed = static_secrets(:acme_managed_gmail_secret)
        get api_v1_secrets_path(limit: 200)
        rows = json_body["data"].select { |s| s["managed_by"].present? }
        assert_equal [ managed.oid ], rows.map { |s| s["id"] }
        assert_equal managed.broker_credential.oid, rows.first.dig("managed_by", "broker_credential_id")
      end

      # -- detail --------------------------------------------------------------

      test "secret detail marks the form kinds editable and the others read-only" do
        get api_v1_secret_detail_path("static", static_secrets(:acme_prod_api_key).oid)
        assert_response :ok
        assert_equal true, json_body.dig("data", "editable")

        get api_v1_secret_detail_path("oauth_token", oauth_token_secrets(:acme_gmail_oauth).oid)
        assert_response :ok
        assert_equal false, json_body.dig("data", "editable")
      end

      test "secret detail shows the full source reference with field roles" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        get api_v1_secret_detail_path("oauth_token", secret.oid)
        assert_equal secret.name, json_body.dig("data", "name")
        segments = json_body.dig("data", "source_segments")
        assert_includes segments, { "role" => "client_id", "type" => "env", "type_label" => "Env", "ref" => "GMAIL_CLIENT_ID" }
        assert_includes segments.map { |s| s["ref"] }, "op://eng/gmail/refresh-token"
        assert_equal segments.map { |s| s["role"] }.sort, segments.map { |s| s["role"] }
      end

      test "secret detail shows an inline source as inline and never its value" do
        secret = StaticSecret.create!(name: "inline-detail", inject_config: { "header" => "X-Key" },
                                      source: SecretSource.new(source_type: "control_plane", secret: "plaintext-value-123"))
        get api_v1_secret_detail_path("static", secret.oid)
        assert_response :ok
        assert_equal [ { "role" => nil, "type" => "control_plane", "type_label" => "Inline", "ref" => "inline" } ],
                     json_body.dig("data", "source_segments")
        assert_no_match "plaintext-value-123", response.body

        get api_v1_secrets_path(q: "inline-detail")
        assert_equal [ "Inline" ], json_body["data"].first["source_types"]
        assert_no_match "plaintext-value-123", response.body
      end

      test "secret detail renders for every secret kind" do
        [
          [ "static", static_secrets(:github_token_inject) ],
          [ "gcp_auth", gcp_auth_secrets(:acme_gcs_keyfile) ],
          [ "gcp_auth", gcp_auth_secrets(:acme_bigquery) ],
          [ "gcp_id_token", gcp_id_token_secrets(:acme_cloud_run) ],
          [ "oauth_token", oauth_token_secrets(:acme_gmail_oauth) ],
          [ "pg_dsn", pg_dsn_secrets(:acme_analytics_pg) ],
          [ "hmac", hmac_secrets(:acme_webhook_hmac) ]
        ].each do |kind, secret|
          get api_v1_secret_detail_path(kind, secret.oid)
          assert_response :ok, "expected #{kind} detail for #{secret.oid}"
          assert_equal kind, json_body.dig("data", "kind")
          assert_equal secret.oid, json_body.dig("data", "id")
        end
      end

      test "gcp auth detail shows the workload identity provider as its source" do
        get api_v1_secret_detail_path("gcp_auth", gcp_auth_secrets(:acme_bigquery).oid)
        assert_equal [ { "role" => nil, "type" => "workload_identity", "type_label" => "Workload-Identity", "ref" => nil } ],
                     json_body.dig("data", "source_segments")
        assert_includes json_body.dig("data", "configuration"), { "label" => "Injects into", "value" => "Authorization: Bearer" }
        assert_includes json_body.dig("data", "configuration"),
                        { "label" => "Scopes", "value" => "https://www.googleapis.com/auth/cloud-platform" }
      end

      test "gcp_id_token detail lists audience, header, and keyfile source" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        get api_v1_secret_detail_path("gcp_id_token", secret.oid)
        config = json_body.dig("data", "configuration")
        assert_includes config, { "label" => "Audience", "value" => secret.audience }
        assert_includes config, { "label" => "Header", "value" => "x-serverless-authorization" }
        assert_includes config, { "label" => "Injects into", "value" => "x-serverless-authorization: Bearer" }
        assert_equal [ "CLOUD_RUN_SA_KEYFILE" ], json_body.dig("data", "source_segments").map { |s| s["ref"] }
        assert_kind_of Array, json_body.dig("data", "rules")
      end

      test "static detail shows profile, mode, injection, and request rules" do
        secret = StaticSecret.create!(
          name: "detail-static", inject_config: { "query_param" => "api_key" },
          rules: [ RequestRule.new(host: "api.example.com", http_methods: [ "GET" ], paths: [ "/v1/*" ], position: 0),
                   RequestRule.new(cidr: "10.0.0.0/8", position: 1) ]
        )
        get api_v1_secret_detail_path("static", secret.oid)
        data = json_body["data"]
        assert_equal [ { "label" => "Injects into", "value" => "?api_key" },
                       { "label" => "Credential profile", "value" => "custom" },
                       { "label" => "Mode", "value" => "inject" } ], data["configuration"]
        assert_equal "?api_key", data["injection"]
        assert_equal [ "api.example.com", nil ], data["rules"].map { |r| r["host"] }
        assert_equal [ "/v1/*" ], data["rules"].first["paths"]
        assert_equal "10.0.0.0/8", data["rules"].last["cidr"]
        assert_nil data["signed_headers"]
      end

      test "pg_dsn detail lists database, role, and session settings without rules" do
        secret = pg_dsn_secrets(:acme_analytics_pg)
        secret.update!(settings: [ { "name" => "app.tenant", "value" => "centaur" },
                                   { "name" => "app.who", "value_from" => { "principal_field" => "foreign_id" } } ])
        get api_v1_secret_detail_path("pg_dsn", secret.oid)
        data = json_body["data"]
        assert_includes data["configuration"], { "label" => "Database", "value" => "analytics" }
        assert_includes data["configuration"], { "label" => "Role", "value" => "readonly" }
        assert_includes data["configuration"],
                        { "label" => "Session settings", "value" => "app.tenant = centaur\napp.who = principal_field(foreign_id)" }
        assert_nil data["injection"]
        assert_nil data["rules"]
      end

      test "hmac detail lists signing configuration and signed headers" do
        get api_v1_secret_detail_path("hmac", hmac_secrets(:acme_webhook_hmac).oid)
        data = json_body["data"]
        assert_equal "X-Signature, X-Timestamp", data["injection"]
        assert_includes data["configuration"], { "label" => "Algorithm", "value" => "sha256" }
        assert_equal [ { "name" => "X-Signature", "value" => "{{ .Signature }}" },
                       { "name" => "X-Timestamp", "value" => "{{ .Timestamp }}" } ], data["signed_headers"]
      end

      test "managed secret detail links to the credential it wraps" do
        secret = static_secrets(:acme_managed_gmail_secret)
        get api_v1_secret_detail_path("static", secret.oid)
        assert_equal secret.broker_credential.oid, json_body.dig("data", "managed_by", "broker_credential_id")
        assert_nil static_payload_managed_by(static_secrets(:acme_prod_api_key))
      end

      test "secret detail 404s for an unknown kind or id" do
        get api_v1_secret_detail_path("bogus", "ssr_whatever")
        assert_response :not_found
        get api_v1_secret_detail_path("static", "ssr_missing")
        assert_response :not_found
        # A kind's oid under another kind is not found either.
        get api_v1_secret_detail_path("pg_dsn", static_secrets(:acme_prod_api_key).oid)
        assert_response :not_found
        get api_v1_secret_role_grants_path("bogus", "ssr_whatever")
        assert_response :not_found
      end

      # -- role grants -----------------------------------------------------------

      test "role grants list the secret's role grants and the assignable roles" do
        secret = static_secrets(:acme_prod_api_key)
        grant = grants(:acme_infra_prod_api_key)
        get api_v1_secret_role_grants_path("static", secret.oid)
        assert_response :ok
        assert_equal [ grant.oid ], json_body["data"].map { |g| g["id"] }
        assert_equal({ "id" => roles(:acme_infra).oid, "foreign_id" => roles(:acme_infra).foreign_id, "name" => "Infra" },
                     json_body["data"].first["role"])
        assignable = json_body["assignable_roles"].map { |r| r["id"] }
        assert_includes assignable, roles(:acme_admin_role).oid
        assert_includes assignable, roles(:globex_infra).oid
        assert_not_includes assignable, roles(:acme_infra).oid
      end

      test "role grants exclude the secret's direct principal grants" do
        secret = static_secrets(:github_token_inject) # granted directly to acme_channel
        assert Grant.where(static_secret: secret).where.not(principal_id: nil).exists?
        get api_v1_secret_role_grants_path("static", secret.oid)
        assert_equal [], json_body["data"].select { |g| g["role_id"].nil? }
        assert json_body["data"].all? { |g| g["role_id"].present? }
      end

      test "assigning a role grants the secret at the default role priority" do
        secret = static_secrets(:acme_staging_api_key)
        role = roles(:acme_admin_role)
        assert_difference -> { role.grants.count }, 1 do
          post api_v1_secret_role_grants_path("static", secret.oid), params: { data: { role_id: role.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :created
        grant = role.grants.find_by!(static_secret: secret)
        assert_equal Grant::DEFAULT_ROLE_PRIORITY, grant.priority
        assert_equal grant.oid, json_body.dig("data", "id")
        assert_equal "Admin", json_body.dig("data", "role", "name")
      end

      test "assigning a role is idempotent" do
        secret = static_secrets(:acme_prod_api_key)
        role = roles(:acme_infra)
        assert_no_difference -> { role.grants.count } do
          post api_v1_secret_role_grants_path("static", secret.oid), params: { data: { role_id: role.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :ok
        assert_equal grants(:acme_infra_prod_api_key).oid, json_body.dig("data", "id")
      end

      test "assigning allows any role and works for every kind" do
        role = roles(:globex_infra)
        assert_difference -> { Grant.count } => 2 do
          post api_v1_secret_role_grants_path("static", static_secrets(:acme_staging_api_key).oid),
               params: { data: { role_id: role.oid } }, headers: csrf_headers, as: :json
          assert_response :created
          post api_v1_secret_role_grants_path("pg_dsn", pg_dsn_secrets(:acme_reporting_pg).oid),
               params: { data: { role_id: role.oid } }, headers: csrf_headers, as: :json
          assert_response :created
        end
        assert Grant.exists?(role: role, pg_dsn_secret: pg_dsn_secrets(:acme_reporting_pg))
      end

      test "assigning an unknown role is a 404" do
        post api_v1_secret_role_grants_path("static", static_secrets(:acme_staging_api_key).oid),
             params: { data: { role_id: "rol_missing" } }, headers: csrf_headers, as: :json
        assert_response :not_found
      end

      test "unassigning removes a role grant from the secret" do
        secret = static_secrets(:acme_prod_api_key)
        grant = grants(:acme_infra_prod_api_key)
        assert_difference -> { roles(:acme_infra).grants.count }, -1 do
          delete api_v1_secret_role_grant_path("static", secret.oid, grant.oid), headers: csrf_headers
        end
        assert_response :no_content
        assert_not Grant.exists?(grant.id)
      end

      test "unassigning only reaches role grants of this secret" do
        grant = grants(:acme_infra_prod_api_key)
        other = static_secrets(:acme_staging_api_key)
        delete api_v1_secret_role_grant_path("static", other.oid, grant.oid), headers: csrf_headers
        assert_response :not_found
        assert Grant.exists?(grant.id)

        direct = Grant.where(static_secret: static_secrets(:github_token_inject)).where.not(principal_id: nil).first!
        delete api_v1_secret_role_grant_path("static", static_secrets(:github_token_inject).oid, direct.oid),
               headers: csrf_headers
        assert_response :not_found
        assert Grant.exists?(direct.id)
      end

      # -- static forms ------------------------------------------------------------

      test "creating a static secret stores its source, rules, and labels" do
        assert_difference -> { StaticSecret.count } => 1, -> { SecretSource.count } => 1, -> { RequestRule.count } => 2 do
          post api_v1_static_secrets_path, params: { data: {
            name: "ui-static", foreign_id: "ui-static", kind: "custom",
            inject_config: { header: "Authorization", formatter: "Bearer {{ .Value }}" },
            source: { source_type: "env", config: { var: "UI_TOKEN" } },
            rules: [ { host: "api.example.com", http_methods: %w[GET POST], paths: [ "/v1/*" ] },
                     { host: "api2.example.com", http_methods: [ "POST" ], paths: [] } ],
            labels: { team: "platform" }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :created
        secret = StaticSecret.find_by!(foreign_id: "ui-static")
        assert_equal secret.oid, json_body.dig("data", "id")
        assert_equal secret.created_by, @operator
        assert_equal({ "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }, secret.inject_config)
        assert_equal({ "team" => "platform" }, secret.labels)
        assert_equal({ "var" => "UI_TOKEN" }, secret.source.config)
        assert_equal [ 0, 1 ], secret.rules.order(:position).map(&:position)
        assert_equal %w[GET POST], secret.rules.order(:position).first.http_methods
      end

      test "creating a static secret with no inject or replace is rejected without writing" do
        assert_no_difference [ "StaticSecret.count", "SecretSource.count", "RequestRule.count" ] do
          post api_v1_static_secrets_path, params: { data: {
            name: "broken", source: { source_type: "env", config: { var: "X" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "base").present?
      end

      test "creating a static secret resolves the GitHub token profile" do
        assert_difference -> { StaticSecret.count } => 1, -> { RequestRule.count } => 3 do
          post api_v1_static_secrets_path, params: { data: {
            name: "GitHub token", foreign_id: "github-token", kind: "github_token",
            source: { source_type: "env", config: { var: "GITHUB_TOKEN" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :created
        secret = StaticSecret.find_by!(foreign_id: "github-token")
        assert_equal "github_token", secret.kind
        assert_nil secret.inject_config
        assert_equal CredentialProfiles::GithubToken::REPLACE_CONFIG, secret.replace_config
        assert_equal %w[api.github.com github.com api.githubcopilot.com], secret.rules.map(&:host)
      end

      test "editing preserves a GitHub token profile through the form representation" do
        secret = StaticSecret.create!(
          name: "GitHub token", kind: "github_token",
          replace_config: CredentialProfiles::GithubToken::REPLACE_CONFIG,
          rules: CredentialProfiles::GithubToken::RULE_ATTRIBUTES.map { |attrs| RequestRule.new(attrs) }
        )
        patch api_v1_static_secret_path(secret.oid), params: { data: {
          name: "renamed GitHub token", kind: "github_token",
          replace_config: { proxy_value: "GITHUB_TOKEN", match_headers: [ "Authorization" ] },
          rules: [ { host: "api.github.com" }, { host: "github.com" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "renamed GitHub token", secret.reload.name
        assert_equal CredentialProfiles::GithubToken::REPLACE_CONFIG, secret.replace_config
      end

      test "an invalid nested rule is rejected without writing and reported by row" do
        assert_no_difference [ "StaticSecret.count", "RequestRule.count" ] do
          post api_v1_static_secrets_path, params: { data: {
            name: "bad-rule", inject_config: { header: "Authorization" },
            rules: [ { host: "h.example.com", cidr: "10.0.0.0/8" } ]
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details").keys.any? { |key| key.start_with?("rules[0].") }
      end

      test "an invalid nested source is rejected without writing and reported under source" do
        assert_no_difference [ "StaticSecret.count", "SecretSource.count" ] do
          post api_v1_static_secrets_path, params: { data: {
            name: "bad-source", inject_config: { header: "Authorization" },
            source: { source_type: "env", config: {} }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details").keys.any? { |key| key.start_with?("source.") }
      end

      test "editing a static secret changes attributes and replaces rules" do
        secret = static_secrets(:github_token_inject)
        patch api_v1_static_secret_path(secret.oid), params: { data: {
          name: "renamed", kind: "custom", inject_config: { header: "X-Token" },
          source: { source_type: "env", config: { var: "NEW_VAR" } },
          rules: [ { host: "only.example.com", http_methods: [ "GET" ], paths: [ "/" ] } ]
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        secret.reload
        assert_equal "renamed", secret.name
        assert_equal({ "header" => "X-Token" }, secret.inject_config)
        assert_equal "NEW_VAR", secret.source.config["var"]
        assert_equal [ "only.example.com" ], secret.rules.map(&:host)
      end

      test "editing an inline source without a new value keeps the stored value" do
        secret = StaticSecret.create!(name: "inline-keep", inject_config: { "header" => "X-Key" },
                                      source: SecretSource.new(source_type: "control_plane", secret: "stored-value"))
        patch api_v1_static_secret_path(secret.oid), params: { data: {
          name: "inline-kept", inject_config: { header: "X-Key" },
          source: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_no_match "stored-value", response.body
        secret.reload
        assert_equal "inline-kept", secret.name
        assert_equal "stored-value", secret.source.secret

        patch api_v1_static_secret_path(secret.oid), params: { data: {
          name: "inline-kept", inject_config: { header: "X-Key" },
          source: { source_type: "control_plane", secret: "new-value", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "new-value", secret.reload.source.secret
      end

      test "switching to an inline source requires a value" do
        secret = static_secrets(:acme_prod_api_key)
        patch api_v1_static_secret_path(secret.oid), params: { data: {
          name: secret.name, inject_config: secret.inject_config, replace_config: secret.replace_config,
          source: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details").key?("source.secret")
      end

      test "PUT keeps replacing the whole document and still requires an inline value" do
        secret = StaticSecret.create!(name: "inline-put", inject_config: { "header" => "X-Key" },
                                      source: SecretSource.new(source_type: "control_plane", secret: "stored-value"))
        put api_v1_static_secret_path(secret.oid), params: { data: {
          name: "inline-put", inject_config: { header: "X-Key" }, source: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal "stored-value", secret.reload.source.secret
      end

      test "deleting a static secret removes it and cascades its grants" do
        secret = static_secrets(:github_token_inject) # granted directly to acme_channel
        assert_difference -> { StaticSecret.count } => -1, -> { Grant.count } => -1 do
          delete api_v1_static_secret_path(secret.oid), headers: csrf_headers
        end
        assert_response :no_content
        assert_not StaticSecret.exists?(secret.id)
      end

      # -- pg_dsn forms -----------------------------------------------------------

      test "creating a pg_dsn secret with an inline DSN source" do
        assert_difference -> { PgDsnSecret.count } => 1, -> { SecretSource.count } => 1 do
          post api_v1_pg_dsn_secrets_path, params: { data: {
            foreign_id: "ui-shop", database: "shop", role: "readonly", settings: [],
            dsn: { source_type: "control_plane", secret: "postgres://u:p@db.example:5432/shop", config: {} }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :created
        assert_no_match "u:p@db", response.body
        secret = PgDsnSecret.find_by!(foreign_id: "ui-shop")
        assert_equal "shop", secret.database
        assert_equal "control_plane", secret.dsn_source.source_type
      end

      test "creating a pg_dsn secret rejects a database that mismatches the inline DSN" do
        assert_no_difference [ "PgDsnSecret.count", "SecretSource.count" ] do
          post api_v1_pg_dsn_secrets_path, params: { data: {
            foreign_id: "ui-mismatch", database: "wrong",
            dsn: { source_type: "control_plane", secret: "postgres://u:p@db.example/shop", config: {} }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
      end

      test "creating a pg_dsn secret requires foreign_id and database" do
        assert_no_difference "PgDsnSecret.count" do
          post api_v1_pg_dsn_secrets_path, params: { data: {
            foreign_id: nil, database: nil, dsn: { source_type: "env", config: { var: "PG_DSN" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "foreign_id").present?
        assert json_body.dig("error", "details", "database").present?
      end

      test "editing a pg_dsn secret changes the database and source" do
        secret = pg_dsn_secrets(:acme_reporting_pg)
        patch api_v1_pg_dsn_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, database: "reporting", role: nil, settings: [],
          dsn: { source_type: "env", config: { var: "REPORTING_DSN" } }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        secret.reload
        assert_nil secret.role.presence
        assert_equal "REPORTING_DSN", secret.dsn_source.config["var"]
      end

      test "editing an inline DSN without a new value keeps it" do
        secret = PgDsnSecret.create!(foreign_id: "inline-dsn", database: "shop", created_by: @operator,
                                     dsn_source: SecretSource.new(source_type: "control_plane",
                                                                  secret: "postgres://u:p@db.example/shop"))
        patch api_v1_pg_dsn_secret_path(secret.oid), params: { data: {
          foreign_id: "inline-dsn", database: "shop", role: "reader", settings: [],
          dsn: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_no_match "u:p@db", response.body
        secret.reload
        assert_equal "reader", secret.role
        assert_equal "postgres://u:p@db.example/shop", secret.dsn_source.secret
      end

      test "deleting a pg_dsn secret" do
        secret = pg_dsn_secrets(:acme_reporting_pg)
        assert_difference -> { PgDsnSecret.count } => -1 do
          delete api_v1_pg_dsn_secret_path(secret.oid), headers: csrf_headers
        end
        assert_response :no_content
      end

      test "creating a pg_dsn secret keeps ordered literal and derived session settings" do
        post api_v1_pg_dsn_secrets_path, params: { data: {
          foreign_id: "ui-settings", database: "settingsdb",
          settings: [
            { name: "app.tenant", value: "centaur" },
            { name: "centaur.slack_channel_id", value_from: { principal_field: "slack_channel_id" } },
            { name: "centaur.principal", value_from: { principal_field: "foreign_id" } },
            { name: "centaur.slack_user_id", value_from: { proxy_label: "centaur.slack_user_id" } },
            { name: "app.region", value: "us" }
          ],
          dsn: { source_type: "env", config: { var: "SETTINGS_DSN" } }
        } }, headers: csrf_headers, as: :json
        assert_response :created
        assert_equal(
          [
            { "name" => "app.tenant", "value" => "centaur" },
            { "name" => "centaur.slack_channel_id", "value_from" => { "principal_field" => "slack_channel_id" } },
            { "name" => "centaur.principal", "value_from" => { "principal_field" => "foreign_id" } },
            { "name" => "centaur.slack_user_id", "value_from" => { "proxy_label" => "centaur.slack_user_id" } },
            { "name" => "app.region", "value" => "us" }
          ],
          PgDsnSecret.find_by!(foreign_id: "ui-settings").settings
        )
      end

      test "creating a pg_dsn secret rejects an unknown principal_field" do
        assert_no_difference "PgDsnSecret.count" do
          post api_v1_pg_dsn_secrets_path, params: { data: {
            foreign_id: "ui-bad-field", database: "badfielddb",
            settings: [ { name: "app.tenant", value_from: { principal_field: "labels" } } ],
            dsn: { source_type: "env", config: { var: "SETTINGS_DSN" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
      end

      test "editing a pg_dsn secret with no settings clears them" do
        secret = pg_dsn_secrets(:acme_reporting_pg)
        secret.update!(settings: [ { "name" => "app.tenant", "value" => "centaur" } ])
        patch api_v1_pg_dsn_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, database: "reporting", settings: [],
          dsn: { source_type: "env", config: { var: "REPORTING_DSN" } }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal [], secret.reload.settings
      end

      test "creating a pg_dsn secret rejects an invalid session setting name" do
        assert_no_difference "PgDsnSecret.count" do
          post api_v1_pg_dsn_secrets_path, params: { data: {
            foreign_id: "ui-bad-setting", database: "badsettingdb",
            settings: [ { name: "session_authorization", value: "x" } ],
            dsn: { source_type: "env", config: { var: "SETTINGS_DSN" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
      end

      # -- gcp_auth forms ---------------------------------------------------------

      test "creating a keyfile gcp_auth secret with a source and rules" do
        assert_difference -> { GcpAuthSecret.count } => 1, -> { SecretSource.count } => 1, -> { RequestRule.count } => 1 do
          post api_v1_gcp_auth_secrets_path, params: { data: {
            foreign_id: "ui-gcp-key", name: "ui-gcp", subject: "bot@acme.example", credentials_provider: nil,
            scopes: %w[https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/devstorage.read_only],
            keyfile: { source_type: "env", config: { var: "GCP_KEY" } },
            rules: [ { host: "storage.googleapis.com", http_methods: [ "GET" ], paths: [ "/" ] } ]
          } }, headers: csrf_headers, as: :json
        end
        assert_response :created
        secret = GcpAuthSecret.find_by!(foreign_id: "ui-gcp-key")
        assert_equal %w[https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/devstorage.read_only], secret.scopes
        assert_equal "bot@acme.example", secret.subject
        assert_nil secret.credentials_provider
        assert_equal({ "var" => "GCP_KEY" }, secret.keyfile_source.config)
        assert_equal [ "storage.googleapis.com" ], secret.rules.map(&:host)
      end

      test "creating a workload-identity gcp_auth secret stores no keyfile" do
        assert_difference -> { GcpAuthSecret.count } => 1 do
          assert_no_difference "SecretSource.count" do
            post api_v1_gcp_auth_secrets_path, params: { data: {
              foreign_id: "ui-gcp-wi", credentials_provider: { type: "workload_identity" },
              scopes: [ "https://www.googleapis.com/auth/cloud-platform" ], rules: []
            } }, headers: csrf_headers, as: :json
          end
        end
        assert_response :created
        secret = GcpAuthSecret.find_by!(foreign_id: "ui-gcp-wi")
        assert_equal({ "type" => "workload_identity" }, secret.credentials_provider)
        assert_nil secret.keyfile_source
        assert_nil secret.subject
      end

      test "creating a gcp_auth secret without scopes is rejected without writing" do
        assert_no_difference [ "GcpAuthSecret.count", "SecretSource.count" ] do
          post api_v1_gcp_auth_secrets_path, params: { data: {
            name: "no-scopes", scopes: [], keyfile: { source_type: "env", config: { var: "GCP_KEY" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "scopes").present?
      end

      test "editing switches a keyfile gcp_auth secret to workload identity" do
        secret = gcp_auth_secrets(:acme_gcs_keyfile)
        assert secret.keyfile_source.present?
        patch api_v1_gcp_auth_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, credentials_provider: { type: "workload_identity" }, subject: nil,
          scopes: [ "https://www.googleapis.com/auth/cloud-platform" ], rules: []
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        secret.reload
        assert_nil secret.keyfile_source
        assert_nil secret.subject
        assert_equal({ "type" => "workload_identity" }, secret.credentials_provider)
      end

      # -- gcp_id_token forms -----------------------------------------------------

      test "creating a gcp_id_token secret with a keyfile source and rule" do
        assert_difference -> { GcpIdTokenSecret.count } => 1, -> { SecretSource.count } => 1, -> { RequestRule.count } => 1 do
          post api_v1_gcp_id_token_secrets_path, params: { data: {
            foreign_id: "ui-cloud-run", name: "ui cloud run",
            audience: "https://ui-service-abc123-uc.a.run.app", header: "x-serverless-authorization",
            keyfile: { source_type: "env", config: { var: "UI_CLOUD_RUN_KEYFILE" } },
            rules: [ { host: "ui-service-abc123-uc.a.run.app", http_methods: [ "GET" ], paths: [ "/" ] } ]
          } }, headers: csrf_headers, as: :json
        end
        assert_response :created
        secret = GcpIdTokenSecret.find_by!(foreign_id: "ui-cloud-run")
        assert_equal "https://ui-service-abc123-uc.a.run.app", secret.audience
        assert_equal "x-serverless-authorization", secret.header
        assert_equal({ "var" => "UI_CLOUD_RUN_KEYFILE" }, secret.keyfile_source.config)
        assert_equal [ "ui-service-abc123-uc.a.run.app" ], secret.rules.map(&:host)
      end

      test "creating a gcp_id_token secret without rules is rejected without writing" do
        assert_no_difference [ "GcpIdTokenSecret.count", "SecretSource.count", "RequestRule.count" ] do
          post api_v1_gcp_id_token_secrets_path, params: { data: {
            foreign_id: "ui-cloud-run-no-rules", audience: "https://ui-service-abc123-uc.a.run.app",
            keyfile: { source_type: "env", config: { var: "UI_CLOUD_RUN_KEYFILE" } }, rules: []
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert_includes json_body.dig("error", "details", "rules"), "must include at least one rule"
      end

      test "editing a gcp_id_token secret changes keyfile, audience, header, and rules" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        patch api_v1_gcp_id_token_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, audience: "https://updated-service-abc123-uc.a.run.app", header: nil,
          keyfile: { source_type: "env", config: { var: "UPDATED_CLOUD_RUN_KEYFILE" } },
          rules: [ { host: "updated-service-abc123-uc.a.run.app", http_methods: [ "POST" ], paths: [] } ]
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        secret.reload
        assert_equal "https://updated-service-abc123-uc.a.run.app", secret.audience
        assert_nil secret.header
        assert_equal "UPDATED_CLOUD_RUN_KEYFILE", secret.keyfile_source.config["var"]
        assert_equal [ "updated-service-abc123-uc.a.run.app" ], secret.rules.map(&:host)
      end

      test "editing an inline gcp keyfile without a new value keeps it" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        secret.keyfile_source.destroy!
        secret.reload.update!(keyfile_source: SecretSource.new(source_type: "control_plane", secret: "{\"type\":\"service_account\"}"))
        patch api_v1_gcp_id_token_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, audience: secret.audience, header: secret.header,
          keyfile: { source_type: "control_plane", config: {} },
          rules: [ { host: "my-service-abc123-uc.a.run.app" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :ok, response.body
        assert_no_match "service_account", response.body
        assert_equal "{\"type\":\"service_account\"}", secret.reload.keyfile_source.secret
      end

      private

      def static_payload_managed_by(secret)
        get api_v1_secret_detail_path("static", secret.oid)
        json_body.dig("data", "managed_by")
      end
    end
  end
end
