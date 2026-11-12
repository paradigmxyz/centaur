require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does.
    class ConsolePrincipalsTest < ActionDispatch::IntegrationTest
      setup do
        @operator = users(:acme_admin)
        sign_in_console(@operator)
      end

      test "principal endpoints are admin-only for every caller type" do
        principal = principals(:acme_user_bob)
        assert_admin_only_api :get, console_api_v1_principals_path
        assert_admin_only_api :get, console_api_v1_principal_path(principal.oid)
        assert_admin_only_api :get, effective_grants_api_v1_principal_path(principal.oid)
        assert_admin_only_api :delete, api_v1_principal_path(principal.oid)
        assert_admin_only_api :put, slack_channel_permissions_api_v1_principal_path(principal.oid),
                              params: { data: { slack_channel_permissions: [] } }
        assert_admin_only_api :post, api_v1_principal_roles_path(principal.oid),
                              params: { data: { role_id: roles(:acme_admin_role).oid } }
        assert_admin_only_api :get, api_v1_slack_channels_path
        assert Principal.exists?(principal.id)
        assert_not principal.reload.roles.include?(roles(:acme_admin_role))
      end

      test "an unknown principal oid is a 404" do
        get console_api_v1_principal_path("prn_missing")
        assert_response :not_found
        post api_v1_principal_roles_path("prn_missing"), params: { data: { role_id: roles(:acme_infra).oid } },
             headers: csrf_headers, as: :json
        assert_response :not_found
      end

      test "index searches names and foreign ids case insensitively" do
        principal = principals(:acme_channel)
        principal.update!(name: "Platform Release Captain")

        get console_api_v1_principals_path(q: "release captain", sort: "name")
        assert_response :ok
        assert_equal [ principal.oid ], json_body["data"].map { |p| p["id"] }

        get console_api_v1_principals_path(q: "c012345", sort: "name")
        assert_equal [ principal.oid ], json_body["data"].map { |p| p["id"] }
        assert_equal 1, json_body.dig("meta", "total")
      end

      test "index sorted by name puts unnamed principals last" do
        principals(:acme_channel).update!(name: "zulu")
        principals(:globex_user).update!(name: "Alpha")
        principals(:acme_user_alice).update!(name: "bravo")

        get console_api_v1_principals_path(sort: "name")
        names = json_body["data"].map { |p| p["name"] }
        assert_equal [ "Alpha", "bravo", "zulu", nil, nil ], names
      end

      test "index sorted by name uses creation time as secondary ordering" do
        common = { kind: "user", name: "same", created_by: @operator }
        newer = Principal.create!(common.merge(foreign_id: "Alpha-sort", created_at: 1.day.ago))
        older = Principal.create!(common.merge(foreign_id: "zeta-sort", created_at: 2.days.ago))

        get console_api_v1_principals_path(sort: "name")
        same = json_body["data"].select { |p| p["name"] == "same" }.map { |p| p["id"] }
        assert_equal [ older.oid, newer.oid ], same
      end

      test "index rejects an unknown sort" do
        get console_api_v1_principals_path(sort: "bogus")
        assert_response :bad_request
      end

      test "index paginates fifty records per page" do
        now = Time.current
        Principal.insert_all!((1..46).map do |number|
          { created_at: now, updated_at: now, created_by_id: @operator.id,
            foreign_id: "pagination-#{number}", kind: "user", name: "Pagination principal #{number}" }
        end)

        get console_api_v1_principals_path(sort: "name")
        assert_equal 50, json_body["data"].size
        assert_equal({ "page" => 1, "limit" => 50, "total" => 51, "total_pages" => 2 }, json_body["meta"])

        get console_api_v1_principals_path(sort: "name", page: 2)
        assert_equal 1, json_body["data"].size
        assert_nil json_body.dig("data", 0, "name")
      end

      test "the console endpoint adds kind, roles, and the DM user id; the bearer endpoint does not" do
        principal = principals(:acme_user_alice)
        principal.update!(slack_user_id: "U0123456789")

        get console_api_v1_principal_path(principal.oid)
        data = json_body["data"]
        assert_equal principal.kind, data["kind"]
        assert_equal principal.slack_user_id, data["slack_user_id"]
        assert_includes data["roles"].map { |r| r["id"] }, roles(:acme_infra).oid
        assert data.key?("inherited_slack_channel_permissions")

        get api_v1_principal_path(principal.oid), headers: bearer_headers
        assert_not json_body["data"].key?("kind")
        assert_not json_body["data"].key?("roles")
      end

      test "labels expose sandbox capabilities but never first-class identity fields" do
        principal = principals(:acme_channel)
        principal.update!(kind: "slack_dm", slack_user_id: "U0123456789", slack_channel_id: "D0123456789",
                          slack_team_id: "T0123456789", slack_email: "member@example.com")

        get console_api_v1_principal_path(principal.oid)
        labels = json_body.dig("data", "labels")
        %w[kind slack_user_id slack_channel_id slack_team_id slack_email].each do |key|
          assert_not labels.key?(key), key
        end
        assert labels.key?(Principal::SANDBOX_REPO_CACHE_LABEL)
      end

      test "create applies default sandbox capabilities and default roles" do
        system_settings(:default).update!(
          default_sandbox_repo_cache: "public",
          default_sandbox_observability_enabled: false,
          default_sandbox_sessions_read_enabled: false,
          default_sandbox_workflows_read_enabled: false,
          default_sandbox_workflows_write_enabled: false
        )
        Role.update_all(assign_by_default: false)
        roles(:acme_infra).update!(assign_by_default: true)
        roles(:globex_infra).update!(assign_by_default: true)

        assert_difference -> { Principal.count }, 1 do
          post api_v1_principals_path,
               params: { data: { foreign_id: "C-new-console", name: "New console principal",
                                 labels: { kind: "slack_channel", team: "platform" } } },
               headers: csrf_headers, as: :json
        end
        assert_response :created

        principal = Principal.find_by!(foreign_id: "C-new-console")
        assert_equal principal.oid, json_body.dig("data", "id")
        assert_equal "New console principal", principal.name
        assert_equal Principal::UNKNOWN_KIND, principal.kind
        assert_equal({ "kind" => "slack_channel", "team" => "platform",
                       Principal::SANDBOX_REPO_CACHE_LABEL => "public" }, principal.labels)
        assert_equal "public", principal.sandbox_repo_cache
        assert_equal false, principal.sandbox_observability_enabled
        assert_equal [ roles(:acme_infra), roles(:globex_infra) ].sort_by(&:id), principal.roles.order(:id).to_a
        assert_equal @operator, principal.created_by
      end

      test "create reports field validation errors" do
        existing = Principal.create!(foreign_id: "duplicate-console-principal", created_by: @operator)

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_path, params: { data: { foreign_id: existing.foreign_id, name: "Duplicate" } },
               headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert_equal "validation failed", json_body.dig("error", "message")
        assert_match(/has already been taken/, json_body.dig("error", "details", "foreign_id").join)
      end

      test "sandbox access is updated with a partial PATCH" do
        principal = principals(:acme_user_bob)
        patch api_v1_principal_path(principal.oid),
              params: { data: { sandbox_repo_cache: "public", sandbox_observability_enabled: false,
                                sandbox_sessions_read_enabled: false, sandbox_workflows_read_enabled: false,
                                sandbox_workflows_write_enabled: false } },
              headers: csrf_headers, as: :json
        assert_response :ok

        principal.reload
        assert_equal "public", principal.sandbox_repo_cache
        assert_equal "public", principal.labels[Principal::SANDBOX_REPO_CACHE_LABEL]
        assert_equal false, principal.sandbox_observability_enabled
        assert_equal false, principal.sandbox_workflows_write_enabled
      end

      test "replacing Slack channel permissions stores the submitted rows" do
        principal = principals(:acme_user_bob)
        put slack_channel_permissions_api_v1_principal_path(principal.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: true, download_enabled: false, history_enabled: true },
              { channel_id: "g9876543210", upload_enabled: false, download_enabled: true, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok

        expected = [
          { "channel_id" => "C0123456789", "upload_enabled" => true, "download_enabled" => false, "history_enabled" => true },
          { "channel_id" => "G9876543210", "upload_enabled" => false, "download_enabled" => true, "history_enabled" => false }
        ]
        assert_equal expected, json_body["data"]
        assert_equal expected, principal.reload.slack_channel_permissions_payload
      end

      test "replacing preserves API-managed DM rows and ignores submitted ones" do
        principal = principals(:acme_user_bob)
        principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        principal.slack_channel_permissions.create!(channel_id: "D0123456789", download_enabled: true)

        put slack_channel_permissions_api_v1_principal_path(principal.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: false, download_enabled: true, history_enabled: false },
              { channel_id: "D0123456789", upload_enabled: true, download_enabled: false, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok

        assert_equal %w[C0123456789 D0123456789], principal.slack_channel_permissions.reload.pluck(:channel_id).sort
        dm = principal.slack_channel_permissions.find_by!(channel_id: "D0123456789")
        assert dm.download_enabled
        assert_not dm.upload_enabled
        assert principal.slack_channel_permissions.find_by!(channel_id: "C0123456789").download_enabled
      end

      test "an unchanged replacement writes nothing" do
        principal = principals(:acme_user_bob)
        channel = principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        dm = principal.slack_channel_permissions.create!(channel_id: "D0123456789", download_enabled: true)
        version = principal.reload.sync_config_cache_version

        put slack_channel_permissions_api_v1_principal_path(principal.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: true, download_enabled: false, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal version, principal.reload.sync_config_cache_version
        assert_equal [ channel.id, dm.id ].sort, principal.slack_channel_permissions.reload.pluck(:id).sort
      end

      test "replacing rejects rows without any permission" do
        principal = principals(:acme_user_bob)
        put slack_channel_permissions_api_v1_principal_path(principal.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: false, download_enabled: false, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_match(/Select at least one Slack permission/, json_body.dig("error", "details").to_s)
      end

      test "a single Slack channel permission can be deleted" do
        principal = principals(:acme_user_bob)
        principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)

        delete slack_channel_permission_api_v1_principal_path(principal.oid, "C0123456789"), headers: csrf_headers
        assert_response :no_content
        assert_empty principal.slack_channel_permissions.reload

        delete slack_channel_permission_api_v1_principal_path(principal.oid, "C0123456789"), headers: csrf_headers
        assert_response :not_found
      end

      test "inherited permissions come from the principal's roles" do
        principal = principals(:acme_channel)
        roles(:acme_infra).slack_channel_permissions.create!(channel_id: "G9876543210", history_enabled: true)

        get console_api_v1_principal_path(principal.oid)
        inherited = json_body.dig("data", "inherited_slack_channel_permissions")
        assert_equal [ "G9876543210" ], inherited.map { |p| p["channel_id"] }
        assert_equal true, inherited.first["history_enabled"]
      end

      test "destroy deletes the principal and dependent access records" do
        principal = principals(:acme_channel)
        proxy = proxies(:acme_proxy)
        client = McpOauthClient.create!(redirect_uris: [ "http://localhost/callback" ])
        McpOauthAuthorizationCode.create!(mcp_oauth_client: client, user: @operator, principal: principal,
                                          redirect_uri: "http://localhost/callback", code_challenge: "challenge",
                                          resource: "https://api.example.test", scopes: %w[mcp:tools])
        McpOauthRefreshToken.create!(mcp_oauth_client: client, user: @operator, principal: principal,
                                     resource: "https://api.example.test", scopes: %w[mcp:tools])

        assert_difference -> { Principal.count }, -1 do
          assert_difference -> { Grant.where(principal: principal).count }, -3 do
            assert_difference -> { PrincipalRole.where(principal: principal).count }, -1 do
              assert_difference -> { McpOauthRefreshToken.where(principal: principal).count }, -1 do
                delete api_v1_principal_path(principal.oid), headers: csrf_headers
              end
            end
          end
        end
        assert_response :no_content
        assert_nil proxy.reload.principal
      end

      test "assigning and unassigning a role" do
        principal = principals(:acme_user_bob)
        role = roles(:acme_admin_role)

        assert_difference -> { principal.principal_roles.count }, 1 do
          post api_v1_principal_roles_path(principal.oid), params: { data: { role_id: role.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :created
        assert_equal role.name, json_body.dig("data", "name")

        assert_no_difference -> { principal.principal_roles.count } do
          post api_v1_principal_roles_path(principal.oid), params: { data: { role_id: role.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :ok

        assert_difference -> { principal.principal_roles.count }, -1 do
          delete api_v1_principal_role_path(principal.oid, role.oid), headers: csrf_headers
        end
        assert_response :no_content
      end

      test "direct grants include the secret's kind, name, and foreign id" do
        principal = principals(:acme_channel)
        secret = static_secrets(:github_token_inject)

        get api_v1_principal_grants_path(principal.oid)
        grant = json_body["data"].find { |g| g["id"] == grants(:acme_channel_github_token).oid }
        assert_equal({ "kind" => "static", "kind_label" => "Static", "id" => secret.oid,
                       "foreign_id" => secret.foreign_id, "name" => secret.name }, grant["grantable"])
        assert_equal secret.oid, grant["static_secret_id"]
      end

      test "granting a secret creates a direct grant at the default priority, idempotently" do
        principal = principals(:acme_user_bob)
        secret = static_secrets(:github_token_inject)

        assert_difference -> { principal.grants.count }, 1 do
          post api_v1_grants_path, params: { data: { principal_id: principal.oid, static_secret_id: secret.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :created
        assert_equal "static", json_body.dig("data", "grantable", "kind")
        assert_equal Grant::DEFAULT_DIRECT_PRIORITY, principal.grants.find_by!(static_secret: secret).priority
        assert_equal @operator, principal.grants.find_by!(static_secret: secret).created_by

        assert_no_difference -> { principal.grants.count } do
          post api_v1_grants_path, params: { data: { principal_id: principal.oid, static_secret_id: secret.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :ok
      end

      test "granting works for a non-static secret kind" do
        principal = principals(:acme_user_bob)
        secret = pg_dsn_secrets(:acme_analytics_pg)
        post api_v1_grants_path, params: { data: { principal_id: principal.oid, pg_dsn_secret_id: secret.oid } },
             headers: csrf_headers, as: :json
        assert_response :created
        assert principal.grants.exists?(pg_dsn_secret: secret)
      end

      test "granting without a secret is a validation error" do
        post api_v1_grants_path, params: { data: { principal_id: principals(:acme_user_bob).oid } },
             headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
      end

      test "revoking a direct grant" do
        principal = principals(:acme_channel)
        grant = grants(:acme_channel_github_token)
        assert_difference -> { principal.grants.count }, -1 do
          delete api_v1_grant_path(grant.oid), headers: csrf_headers
        end
        assert_response :no_content
      end

      test "effective grants source each secret as direct or via a role" do
        principal = principals(:acme_channel)
        get effective_grants_api_v1_principal_path(principal.oid)
        assert_response :ok

        direct = json_body["data"].find { |g| g["id"] == static_secrets(:github_token_inject).oid }
        assert_equal true, direct["direct"]
        assert_equal "static", direct["kind"]
        inherited = json_body["data"].find { |g| g["id"] == static_secrets(:acme_prod_api_key).oid }
        assert_equal false, inherited["direct"]
        assert_equal [ roles(:acme_infra).name ], inherited["roles"].map { |r| r["name"] }
      end

      test "effective grants paginate" do
        principal = principals(:acme_channel)
        now = Time.current
        secret_ids = StaticSecret.insert_all!((1..50).map do |number|
          { foreign_id: "detail-page-secret-#{number}", name: "Detail page secret #{number}", kind: "custom",
            labels: {}, inject_config: { "header" => "X-Detail-Page-#{number}" }, created_by_id: @operator.id,
            created_at: now, updated_at: now }
        end, returning: %w[id]).rows.flatten
        Grant.insert_all!(secret_ids.map do |secret_id|
          { principal_id: principal.id, static_secret_id: secret_id, priority: Grant::DEFAULT_DIRECT_PRIORITY,
            created_by_id: @operator.id, created_at: now, updated_at: now }
        end)

        get effective_grants_api_v1_principal_path(principal.oid, page: 2)
        assert_equal 4, json_body["data"].size
        assert_equal({ "page" => 2, "limit" => 50, "total" => 54, "total_pages" => 2 }, json_body["meta"])

        get api_v1_principal_grants_path(principal.oid, page: 2)
        assert_equal 3, json_body["data"].size
        assert_equal 53, json_body.dig("meta", "total")
      end

      test "secrets lists every kind with identifying metadata only" do
        get api_v1_secrets_path(limit: 200)
        assert_response :ok
        kinds = json_body["data"].map { |s| s["kind"] }.uniq
        assert_includes kinds, "static"
        assert_includes kinds, "gcp_auth"
        assert_includes kinds, "gcp_id_token"
        static = json_body["data"].find { |s| s["id"] == static_secrets(:github_token_inject).oid }
        assert_equal "static_secret_id", static["grant_key"]
        assert_equal %w[created_at description foreign_id grant_key id kind kind_label labels managed_by name source_types updated_at],
                     static.keys.sort
        names = json_body["data"].map { |s| s["name"].to_s.downcase }
        assert_equal names.sort, names
      end

      test "secrets filters by kind and query" do
        secret = static_secrets(:acme_staging_api_key)
        secret.update!(name: "Unique Picker Name")
        get api_v1_secrets_path(kind: "static", q: "picker NAME")
        assert_equal [ secret.oid ], json_body["data"].map { |s| s["id"] }

        get api_v1_secrets_path(kind: "bogus")
        assert_response :bad_request
      end

      test "secrets exclude a principal's direct grants but keep role-inherited ones" do
        principal = principals(:acme_channel)
        get api_v1_secrets_path(exclude_grantee: "principal:#{principal.oid}", limit: 200)
        assert_response :ok
        ids = json_body["data"].map { |s| s["id"] }
        assert_not_includes ids, static_secrets(:github_token_inject).oid
        assert_not_includes ids, static_secrets(:db_password_replace).oid
        assert_not_includes ids, pg_dsn_secrets(:acme_analytics_pg).oid
        # acme_prod_api_key is inherited through the acme_infra role, so it can still be granted directly.
        assert_includes ids, static_secrets(:acme_prod_api_key).oid
      end

      test "secrets exclude a grantee's grants across every page" do
        principal = principals(:acme_user_bob)
        inject = { header: "X-Api-Key", formatter: "{{ .Value }}" }
        granted = Array.new(5) do |i|
          StaticSecret.create!(name: "aaa-paged-#{i}", inject_config: inject,
                               created_by: users(:acme_admin)).tap do |secret|
            principal.grants.create!(static_secret: secret, created_by: users(:acme_admin))
          end
        end
        open_secret = StaticSecret.create!(name: "aaa-paged-open", inject_config: inject, created_by: users(:acme_admin))
        expected_total = StaticSecret.count + GcpAuthSecret.count + GcpIdTokenSecret.count + AwsAuthSecret.count +
                         OauthTokenSecret.count + PgDsnSecret.count + HmacSecret.count + SshKeySecret.count -
                         principal.grants.count

        seen = []
        page = 1
        loop do
          get api_v1_secrets_path(exclude_grantee: "principal:#{principal.oid}", limit: 2, page: page)
          assert_response :ok
          assert_equal expected_total, json_body.dig("meta", "total")
          seen.concat(json_body["data"].map { |s| s["id"] })
          break if page >= json_body.dig("meta", "total_pages")

          page += 1
        end
        assert_equal expected_total, seen.size
        assert_empty seen & granted.map(&:oid)
        assert_includes seen, open_secret.oid
      end

      test "secrets reject an unknown or malformed grantee" do
        get api_v1_secrets_path(exclude_grantee: "principal:prn_missing")
        assert_response :not_found
        get api_v1_secrets_path(exclude_grantee: "user:#{users(:acme_admin).id}")
        assert_response :bad_request
      end

      test "slack channels return bounded catalog matches and exclude an owner's channels" do
        principal = principals(:acme_channel)
        principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        SlackBotChannel.create!(team_id: "T0123456789", bot_user_id: "U0999999999", channel_id: "C1111111111",
                                name: "engineering", active: true, member_user_ids: [ "U0999999999" ])

        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token") do
          get api_v1_slack_channels_path(q: "eng", exclude_owner: "principal:#{principal.oid}")
        end
        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        assert_equal(
          { "data" => [ { "channel_id" => "C1111111111", "name" => "engineering", "private" => false,
                          "label" => "#engineering", "description" => "C1111111111 · Public" } ],
            "meta" => { "catalog_error" => nil } },
          json_body
        )

        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token") do
          get api_v1_slack_channels_path(exclude_owner: "principal:#{principal.oid}")
        end
        assert_not_includes json_body["data"].map { |c| c["channel_id"] }, "C0123456789"
      end

      test "slack channels resolve names for existing rows by id" do
        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token") do
          get api_v1_slack_channels_path(ids: %w[C0123456789 g9876543210 C9999999999])
        end
        assert_equal({ "C0123456789" => "general", "G9876543210" => "private" },
                     json_body["data"].to_h { |c| [ c["channel_id"], c["name"] ] })
      end

      test "slack channels explain a missing bot token" do
        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => nil, "IRON_CONTROL_SLACK_BOT_TOKEN" => nil) do
          get api_v1_slack_channels_path
        end
        assert_equal "SLACK_BOT_TOKEN is not configured.", json_body.dig("meta", "catalog_error")
      end

      test "slack channels reject a malformed owner" do
        get api_v1_slack_channels_path(exclude_owner: "user:usr_1")
        assert_response :bad_request
      end
    end
  end
end
