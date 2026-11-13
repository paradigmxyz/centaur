require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does; bodies
    # are the JSON the SPA forms send.
    class ConsoleSecretsTest < ActionDispatch::IntegrationTest
      setup do
        @operator = users(:acme_admin)
        sign_in_console(@operator)
      end

      test "secret list, detail, and role grant endpoints are admin-only" do
        secret = static_secrets(:acme_prod_api_key)
        grant = grants(:acme_infra_prod_api_key)
        assert_admin_only_api :get, api_v1_console_secrets_path
        assert_admin_only_api :get, api_v1_console_secret_detail_path("static", secret.oid)
        assert_admin_only_api :get, api_v1_console_secret_role_grants_path("static", secret.oid)
        assert_admin_only_api :post, api_v1_console_secret_role_grants_path("static", secret.oid),
                              params: { data: { role_id: roles(:acme_admin_role).oid } }
        assert_admin_only_api :delete, api_v1_console_secret_role_grant_path("static", secret.oid, grant.oid)
        assert Grant.exists?(grant.id)
        assert_not Grant.exists?(role: roles(:acme_admin_role), static_secret: secret)
      end

      test "the per-kind writes the forms use are admin-only" do
        {
          "static" => [ api_v1_console_static_secrets_path, static_secrets(:acme_prod_api_key) ],
          "pg_dsn" => [ api_v1_console_pg_dsn_secrets_path, pg_dsn_secrets(:acme_reporting_pg) ],
          "gcp_auth" => [ api_v1_console_gcp_auth_secrets_path, gcp_auth_secrets(:acme_gcs_keyfile) ],
          "gcp_id_token" => [ api_v1_console_gcp_id_token_secrets_path, gcp_id_token_secrets(:acme_cloud_run) ]
        }.each do |_kind, (collection, record)|
          assert_admin_only_api :get, "#{collection}/#{record.oid}"
          assert_admin_only_api :post, collection, params: { data: { name: "sneaky" } }
          assert_admin_only_api :patch, "#{collection}/#{record.oid}", params: { data: { name: "sneaky" } }
          assert_admin_only_api :delete, "#{collection}/#{record.oid}"
          assert record.class.exists?(record.id)
          assert_not_equal "sneaky", record.reload.name
        end
      end

      test "secrets list combines kinds with backend labels but never references" do
        secret = static_secrets(:acme_prod_api_key)
        get api_v1_console_secrets_path(limit: 200)
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

      test "secrets list paginates fifty filtered records per page" do
        now = Time.current
        StaticSecret.insert_all!((1..51).map do |number|
          { created_at: now, updated_at: now, name: "Pagination secret #{number}", inject_config: { header: "Authorization" } }
        end)

        get api_v1_console_secrets_path(q: "Pagination secret", kind: "static")
        assert_equal 50, json_body["data"].size
        assert_equal({ "page" => 1, "limit" => 50, "total" => 51, "total_pages" => 2 }, json_body["meta"])

        get api_v1_console_secrets_path(q: "Pagination secret", kind: "static", page: 2)
        assert_equal 1, json_body["data"].size
      end

      test "secrets list loads only the requested page, in a fixed number of queries" do
        insert_secrets = lambda do |prefix, count|
          now = Time.current
          StaticSecret.insert_all!((1..count).map do |number|
            { created_at: now, updated_at: now, name: "#{prefix} #{number}", inject_config: { header: "Authorization" } }
          end)
          PgDsnSecret.insert_all!((1..count).map do |number|
            { created_at: now, updated_at: now, name: "#{prefix} pg #{number}", database: "db", created_by_id: @operator.id }
          end)
        end
        insert_secrets.call("Early", 3)
        request = -> { get api_v1_console_secrets_path(limit: 2) }

        assert_queries_do_not_grow(request) { insert_secrets.call("Later", 20) }
        loaded = capture_instantiations(&request)
        assert_equal 2, json_body["data"].size
        assert_operator loaded.slice(*SecretKinds::SECRET_KINDS.values.map { |c| c[:model].name }).values.sum, :<=, 2
      end

      test "secrets list orders by case-insensitive name across kinds, unnamed first" do
        now = Time.current
        StaticSecret.insert_all!([ "beta", "Alpha", nil ].map do |name|
          { created_at: now, updated_at: now, name: name, inject_config: { header: "Authorization" } }
        end)
        PgDsnSecret.insert_all!([ { created_at: now, updated_at: now, name: "ALPHA", database: "db", created_by_id: @operator.id } ])

        get api_v1_console_secrets_path(limit: 200)
        names = json_body["data"].map { |row| row["name"] }
        keys = names.map { |name| name.to_s.downcase }
        assert_equal keys.sort, keys
        assert_nil names.first
      end

      test "secrets list badges an OAuth-flow-managed static secret" do
        managed = static_secrets(:acme_managed_gmail_secret)
        get api_v1_console_secrets_path(limit: 200)
        rows = json_body["data"].select { |s| s["managed_by"].present? }
        assert_equal [ managed.oid ], rows.map { |s| s["id"] }
        assert_equal managed.broker_credential.oid, rows.first.dig("managed_by", "broker_credential_id")
      end

      test "secret detail marks the form kinds editable and the others read-only" do
        get api_v1_console_secret_detail_path("static", static_secrets(:acme_prod_api_key).oid)
        assert_response :ok
        assert_equal true, json_body.dig("data", "editable")

        get api_v1_console_secret_detail_path("oauth_token", oauth_token_secrets(:acme_gmail_oauth).oid)
        assert_response :ok
        assert_equal false, json_body.dig("data", "editable")
      end

      test "secret detail shows the full source reference with field roles" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        get api_v1_console_secret_detail_path("oauth_token", secret.oid)
        assert_equal secret.name, json_body.dig("data", "name")
        segments = json_body.dig("data", "source_segments")
        assert_includes segments, { "role" => "client_id", "type" => "env", "type_label" => "Env", "ref" => "GMAIL_CLIENT_ID" }
        assert_includes segments.map { |s| s["ref"] }, "op://eng/gmail/refresh-token"
        assert_equal segments.map { |s| s["role"] }.sort, segments.map { |s| s["role"] }
      end

      test "secret detail shows an inline source as inline and never its value" do
        secret = StaticSecret.create!(name: "inline-detail", inject_config: { "header" => "X-Key" },
                                      source: SecretSource.new(source_type: "control_plane", secret: "plaintext-value-123"))
        get api_v1_console_secret_detail_path("static", secret.oid)
        assert_response :ok
        assert_equal [ { "role" => nil, "type" => "control_plane", "type_label" => "Inline", "ref" => "inline" } ],
                     json_body.dig("data", "source_segments")
        assert_no_match "plaintext-value-123", response.body

        get api_v1_console_secrets_path(q: "inline-detail")
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
          get api_v1_console_secret_detail_path(kind, secret.oid)
          assert_response :ok, "expected #{kind} detail for #{secret.oid}"
          assert_equal kind, json_body.dig("data", "kind")
          assert_equal secret.oid, json_body.dig("data", "id")
        end
      end

      test "gcp auth detail shows the workload identity provider as its source" do
        get api_v1_console_secret_detail_path("gcp_auth", gcp_auth_secrets(:acme_bigquery).oid)
        assert_equal [ { "role" => nil, "type" => "workload_identity", "type_label" => "Workload-Identity", "ref" => nil } ],
                     json_body.dig("data", "source_segments")
        assert_includes json_body.dig("data", "configuration"), { "label" => "Injects into", "value" => "Authorization: Bearer" }
        assert_includes json_body.dig("data", "configuration"),
                        { "label" => "Scopes", "value" => "https://www.googleapis.com/auth/cloud-platform" }
      end

      test "gcp_id_token detail lists audience, header, and keyfile source" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        get api_v1_console_secret_detail_path("gcp_id_token", secret.oid)
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
        get api_v1_console_secret_detail_path("static", secret.oid)
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
        get api_v1_console_secret_detail_path("pg_dsn", secret.oid)
        data = json_body["data"]
        assert_includes data["configuration"], { "label" => "Database", "value" => "analytics" }
        assert_includes data["configuration"], { "label" => "Role", "value" => "readonly" }
        assert_includes data["configuration"],
                        { "label" => "Session settings", "value" => "app.tenant = centaur\napp.who = principal_field(foreign_id)" }
        assert_nil data["injection"]
        assert_nil data["rules"]
      end

      test "hmac detail lists signing configuration and signed headers" do
        get api_v1_console_secret_detail_path("hmac", hmac_secrets(:acme_webhook_hmac).oid)
        data = json_body["data"]
        assert_equal "X-Signature, X-Timestamp", data["injection"]
        assert_includes data["configuration"], { "label" => "Algorithm", "value" => "sha256" }
        assert_equal [ { "name" => "X-Signature", "value" => "{{ .Signature }}" },
                       { "name" => "X-Timestamp", "value" => "{{ .Timestamp }}" } ], data["signed_headers"]
      end

      test "managed secret detail links to the credential it wraps" do
        secret = static_secrets(:acme_managed_gmail_secret)
        get api_v1_console_secret_detail_path("static", secret.oid)
        assert_equal secret.broker_credential.oid, json_body.dig("data", "managed_by", "broker_credential_id")
        assert_nil static_payload_managed_by(static_secrets(:acme_prod_api_key))
      end

      test "secret detail 404s for an unknown kind or id" do
        get api_v1_console_secret_detail_path("bogus", "ssr_whatever")
        assert_response :not_found
        get api_v1_console_secret_detail_path("static", "ssr_missing")
        assert_response :not_found
        # A kind's oid under another kind is not found either.
        get api_v1_console_secret_detail_path("pg_dsn", static_secrets(:acme_prod_api_key).oid)
        assert_response :not_found
        get api_v1_console_secret_role_grants_path("bogus", "ssr_whatever")
        assert_response :not_found
      end

      test "role grants list the secret's role grants and the assignable roles" do
        secret = static_secrets(:acme_prod_api_key)
        grant = grants(:acme_infra_prod_api_key)
        get api_v1_console_secret_role_grants_path("static", secret.oid)
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
        get api_v1_console_secret_role_grants_path("static", secret.oid)
        assert json_body["data"].all? { |g| g["role_id"].present? }
      end

      test "assigning a role grants the secret at the default role priority" do
        secret = static_secrets(:acme_staging_api_key)
        role = roles(:acme_admin_role)
        assert_difference -> { role.grants.count }, 1 do
          post api_v1_console_secret_role_grants_path("static", secret.oid), params: { data: { role_id: role.oid } },
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
          post api_v1_console_secret_role_grants_path("static", secret.oid), params: { data: { role_id: role.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :ok
        assert_equal grants(:acme_infra_prod_api_key).oid, json_body.dig("data", "id")
      end

      test "assigning allows any role and works for every kind" do
        role = roles(:globex_infra)
        assert_difference -> { Grant.count } => 2 do
          post api_v1_console_secret_role_grants_path("static", static_secrets(:acme_staging_api_key).oid),
               params: { data: { role_id: role.oid } }, headers: csrf_headers, as: :json
          assert_response :created
          post api_v1_console_secret_role_grants_path("pg_dsn", pg_dsn_secrets(:acme_reporting_pg).oid),
               params: { data: { role_id: role.oid } }, headers: csrf_headers, as: :json
          assert_response :created
        end
        assert Grant.exists?(role: role, pg_dsn_secret: pg_dsn_secrets(:acme_reporting_pg))
      end

      test "assigning an unknown role is a 404" do
        post api_v1_console_secret_role_grants_path("static", static_secrets(:acme_staging_api_key).oid),
             params: { data: { role_id: "rol_missing" } }, headers: csrf_headers, as: :json
        assert_response :not_found
      end

      test "unassigning removes a role grant from the secret" do
        secret = static_secrets(:acme_prod_api_key)
        grant = grants(:acme_infra_prod_api_key)
        assert_difference -> { roles(:acme_infra).grants.count }, -1 do
          delete api_v1_console_secret_role_grant_path("static", secret.oid, grant.oid), headers: csrf_headers
        end
        assert_response :no_content
        assert_not Grant.exists?(grant.id)
      end

      test "unassigning only reaches role grants of this secret" do
        grant = grants(:acme_infra_prod_api_key)
        other = static_secrets(:acme_staging_api_key)
        delete api_v1_console_secret_role_grant_path("static", other.oid, grant.oid), headers: csrf_headers
        assert_response :not_found
        assert Grant.exists?(grant.id)

        direct = Grant.where(static_secret: static_secrets(:github_token_inject)).where.not(principal_id: nil).first!
        delete api_v1_console_secret_role_grant_path("static", static_secrets(:github_token_inject).oid, direct.oid),
               headers: csrf_headers
        assert_response :not_found
        assert Grant.exists?(direct.id)
      end

      # Nested validation errors keep the keys earlier versions returned, and
      # add path keys ("rules[1].cidr", "keyfile.config") alongside them.

      test "static nested errors keep the first invalid record's keys and add every path" do
        post api_v1_console_static_secrets_path, params: { data: {
          name: "compat", inject_config: { header: "A" },
          rules: [ { host: "ok.example.com" }, { cidr: "not-a-cidr" }, { host: "h", cidr: "10.0.0.0/8" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        details = json_body.dig("error", "details")
        assert_equal details["rules[1].cidr"], details["cidr"]
        assert details["rules[1].cidr"].present?
        assert_equal [ "host and cidr are mutually exclusive" ], details["rules[2].base"]
        assert_not details.key?("rules[0].host")
      end

      test "static parent errors still take precedence over nested errors" do
        post api_v1_console_static_secrets_path, params: { data: {
          name: "compat", rules: [ { host: "h", cidr: "10.0.0.0/8" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        details = json_body.dig("error", "details")
        assert details["base"].present?
        assert_not details.keys.any? { |key| key.start_with?("rules[") }
      end

      test "gcp create with an invalid rule keeps rules is invalid and the parent's own errors" do
        post api_v1_console_gcp_auth_secrets_path, params: { data: {
          foreign_id: "compat-gcp", scopes: [], keyfile: { source_type: "env", config: { var: "K" } },
          rules: [ { host: "h", cidr: "10.0.0.0/8" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        details = json_body.dig("error", "details")
        assert_equal [ "is invalid" ], details["rules"]
        assert details["scopes"].present?
        assert_equal [ "host and cidr are mutually exclusive" ], details["rules[0].base"]
        assert_not GcpAuthSecret.exists?(foreign_id: "compat-gcp")
      end

      test "gcp_id_token create with an invalid rule keeps rules is invalid" do
        post api_v1_console_gcp_id_token_secrets_path, params: { data: {
          foreign_id: "compat-id", audience: "https://a.example.com",
          keyfile: { source_type: "env", config: { var: "K" } }, rules: [ { host: "h", cidr: "10.0.0.0/8" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal({ "rules" => [ "is invalid" ], "rules[0].base" => [ "host and cidr are mutually exclusive" ] },
                     json_body.dig("error", "details"))
      end

      test "an invalid keyfile or DSN source is a 422 instead of being dropped or failing mid-write" do
        message = [ "is missing required key \"var\" for source_type \"env\"" ]
        assert_no_difference [ "GcpAuthSecret.count", "SecretSource.count" ] do
          post api_v1_console_gcp_auth_secrets_path, params: { data: {
            foreign_id: "compat-key", scopes: [ "x" ], keyfile: { source_type: "env", config: {} }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert_equal({ "keyfile_source" => [ "is invalid" ], "keyfile.config" => message }, json_body.dig("error", "details"))

        secret = pg_dsn_secrets(:acme_reporting_pg)
        patch api_v1_console_pg_dsn_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, database: "reporting", dsn: { source_type: "env", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal({ "dsn_source" => [ "is invalid" ], "dsn.config" => message }, json_body.dig("error", "details"))

        id_token = gcp_id_token_secrets(:acme_cloud_run)
        patch api_v1_console_gcp_id_token_secret_path(id_token.oid), params: { data: {
          foreign_id: id_token.foreign_id, audience: id_token.audience, rules: [ { host: "a" }, { host: "h", cidr: "10.0.0.0/8" } ],
          keyfile: { source_type: "env", config: { var: "K" } }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal({ "rules" => [ "is invalid" ], "rules[1].base" => [ "host and cidr are mutually exclusive" ] },
                     json_body.dig("error", "details"))
        assert_equal "CLOUD_RUN_SA_KEYFILE", id_token.reload.keyfile_source.config["var"]
      end

      test "creating a static secret stores its source, rules, and labels" do
        assert_difference -> { StaticSecret.count } => 1, -> { SecretSource.count } => 1, -> { RequestRule.count } => 2 do
          post api_v1_console_static_secrets_path, params: { data: {
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
          post api_v1_console_static_secrets_path, params: { data: {
            name: "broken", source: { source_type: "env", config: { var: "X" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "base").present?
      end

      test "editing preserves a GitHub token profile through the form representation" do
        secret = StaticSecret.create!(
          name: "GitHub token", kind: "github_token",
          replace_config: CredentialProfiles::GithubToken::REPLACE_CONFIG,
          rules: CredentialProfiles::GithubToken::RULE_ATTRIBUTES.map { |attrs| RequestRule.new(attrs) }
        )
        patch api_v1_console_static_secret_path(secret.oid), params: { data: {
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
          post api_v1_console_static_secrets_path, params: { data: {
            name: "bad-rule", inject_config: { header: "Authorization" },
            rules: [ { host: "h.example.com", cidr: "10.0.0.0/8" } ]
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        # The rule's own keys plus the additive path key.
        assert_equal({ "base" => [ "host and cidr are mutually exclusive" ],
                       "rules[0].base" => [ "host and cidr are mutually exclusive" ] }, json_body.dig("error", "details"))
      end

      test "an invalid nested source is rejected without writing and reported under source" do
        assert_no_difference [ "StaticSecret.count", "SecretSource.count" ] do
          post api_v1_console_static_secrets_path, params: { data: {
            name: "bad-source", inject_config: { header: "Authorization" },
            source: { source_type: "env", config: {} }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        message = [ "is missing required key \"var\" for source_type \"env\"" ]
        assert_equal({ "config" => message, "source.config" => message }, json_body.dig("error", "details"))
      end

      test "editing an inline source without a new value keeps the stored value" do
        secret = StaticSecret.create!(name: "inline-keep", inject_config: { "header" => "X-Key" },
                                      source: SecretSource.new(source_type: "control_plane", secret: "stored-value"))
        patch api_v1_console_static_secret_path(secret.oid), params: { data: {
          name: "inline-kept", inject_config: { header: "X-Key" },
          source: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_no_match "stored-value", response.body
        secret.reload
        assert_equal "inline-kept", secret.name
        assert_equal "stored-value", secret.source.secret

        patch api_v1_console_static_secret_path(secret.oid), params: { data: {
          name: "inline-kept", inject_config: { header: "X-Key" },
          source: { source_type: "control_plane", secret: "new-value", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "new-value", secret.reload.source.secret
      end

      test "switching to an inline source requires a value" do
        secret = static_secrets(:acme_prod_api_key)
        patch api_v1_console_static_secret_path(secret.oid), params: { data: {
          name: secret.name, inject_config: secret.inject_config, replace_config: secret.replace_config,
          source: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        message = [ "can't be blank for source_type \"control_plane\"" ]
        assert_equal({ "secret" => message, "source.secret" => message }, json_body.dig("error", "details"))
      end

      test "PUT keeps replacing the whole document and still requires an inline value" do
        secret = StaticSecret.create!(name: "inline-put", inject_config: { "header" => "X-Key" },
                                      source: SecretSource.new(source_type: "control_plane", secret: "stored-value"))
        put api_v1_console_static_secret_path(secret.oid), params: { data: {
          name: "inline-put", inject_config: { header: "X-Key" }, source: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal "stored-value", secret.reload.source.secret
      end

      test "editing an inline DSN without a new value keeps it" do
        secret = PgDsnSecret.create!(foreign_id: "inline-dsn", database: "shop", created_by: @operator,
                                     dsn_source: SecretSource.new(source_type: "control_plane",
                                                                  secret: "postgres://u:p@db.example/shop"))
        patch api_v1_console_pg_dsn_secret_path(secret.oid), params: { data: {
          foreign_id: "inline-dsn", database: "shop", role: "reader", settings: [],
          dsn: { source_type: "control_plane", config: {} }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_no_match "u:p@db", response.body
        secret.reload
        assert_equal "reader", secret.role
        assert_equal "postgres://u:p@db.example/shop", secret.dsn_source.secret
      end

      test "creating a pg_dsn secret keeps ordered literal and derived session settings" do
        post api_v1_console_pg_dsn_secrets_path, params: { data: {
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

      test "creating a gcp_auth secret without scopes is rejected without writing" do
        assert_no_difference [ "GcpAuthSecret.count", "SecretSource.count" ] do
          post api_v1_console_gcp_auth_secrets_path, params: { data: {
            name: "no-scopes", scopes: [], keyfile: { source_type: "env", config: { var: "GCP_KEY" } }
          } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "scopes").present?
      end

      test "editing an inline gcp keyfile without a new value keeps it" do
        secret = gcp_id_token_secrets(:acme_cloud_run)
        secret.keyfile_source.destroy!
        secret.reload.update!(keyfile_source: SecretSource.new(source_type: "control_plane", secret: "{\"type\":\"service_account\"}"))
        patch api_v1_console_gcp_id_token_secret_path(secret.oid), params: { data: {
          foreign_id: secret.foreign_id, audience: secret.audience, header: secret.header,
          keyfile: { source_type: "control_plane", config: {} },
          rules: [ { host: "my-service-abc123-uc.a.run.app" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :ok, response.body
        assert_no_match "service_account", response.body
        assert_equal "{\"type\":\"service_account\"}", secret.reload.keyfile_source.secret
      end

      test "an explicit null foreign_id clears it on a static secret addressed by OID" do
        secret = StaticSecret.create!(name: "clear-id", foreign_id: "clear-me", inject_config: { "header" => "Authorization" },
                                      source: SecretSource.new(source_type: "env", config: { "var" => "CLEAR_ME" }))
        patch api_v1_console_static_secret_path(secret.oid), params: { data: {
          foreign_id: nil, name: "clear-id", inject_config: { header: "Authorization" },
          source: { source_type: "env", config: { var: "CLEAR_ME" } }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_nil json_body.dig("data", "foreign_id")
        assert_nil secret.reload.foreign_id
      end

      test "an omitted or blank foreign_id keeps it on a static secret addressed by OID" do
        secret = static_secrets(:acme_prod_api_key)
        body = { name: secret.name, inject_config: secret.inject_config, replace_config: secret.replace_config,
                 source: { source_type: "env", config: { var: "PROD_API_KEY" } } }
        [ body, body.merge(foreign_id: ""), body.merge(foreign_id: "  ") ].each do |data|
          patch api_v1_console_static_secret_path(secret.oid), params: { data: data }, headers: csrf_headers, as: :json
          assert_response :ok
          assert_equal "prod-api-key", secret.reload.foreign_id
        end
      end

      test "an explicit null foreign_id keeps the identity of an upsert addressed by foreign_id" do
        secret = static_secrets(:acme_prod_api_key)
        put api_v1_console_static_secret_path("prod-api-key"), params: { data: {
          foreign_id: nil, name: secret.name, inject_config: secret.inject_config, replace_config: secret.replace_config,
          source: { source_type: "env", config: { var: "PROD_API_KEY" } }
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
        assert_equal "prod-api-key", secret.reload.foreign_id
      end

      test "an explicit null foreign_id clears it on gcp_auth and gcp_id_token secrets" do
        gcp_auth = gcp_auth_secrets(:acme_gcs_keyfile)
        patch api_v1_console_gcp_auth_secret_path(gcp_auth.oid), params: { data: {
          foreign_id: nil, credentials_provider: { type: "workload_identity" }, subject: nil,
          scopes: [ "https://www.googleapis.com/auth/cloud-platform" ], rules: []
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_nil gcp_auth.reload.foreign_id

        id_token = gcp_id_token_secrets(:acme_cloud_run)
        patch api_v1_console_gcp_id_token_secret_path(id_token.oid), params: { data: {
          foreign_id: nil, audience: id_token.audience, header: id_token.header,
          keyfile: { source_type: "env", config: { var: "CLOUD_RUN_KEYFILE" } },
          rules: [ { host: "my-service-abc123-uc.a.run.app" } ]
        } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_nil id_token.reload.foreign_id
      end

      test "an explicit null foreign_id on a pg_dsn secret is a 422 because it is required" do
        secret = pg_dsn_secrets(:acme_reporting_pg)
        patch api_v1_console_pg_dsn_secret_path(secret.oid), params: { data: {
          foreign_id: nil, database: secret.database, role: secret.role, settings: [],
          dsn: { source_type: "env", config: { var: "REPORTING_DSN" } }
        } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "foreign_id").present?
        assert_equal "pg-reporting", secret.reload.foreign_id
      end

      private

      def static_payload_managed_by(secret)
        get api_v1_console_secret_detail_path("static", secret.oid)
        json_body.dig("data", "managed_by")
      end
    end
  end
end
