require "test_helper"

module Api
  module V1
    # The /api/v1 surface behind the console role pages, ported from the former
    # HTML controller tests (console/roles_controller_test.rb, the role cases of
    # console/slack_channel_permissions_controller_test.rb and
    # console/slack_channel_options_controller_test.rb, and the role case of
    # console_controller_test.rb). Requests use the console session cookie
    # (+ CSRF) as the SPA does; bearer parity is covered where the endpoint is new.
    class ConsoleRolesTest < ActionDispatch::IntegrationTest
      setup do
        @operator = users(:acme_admin)
        sign_in_console(@operator)
      end

      # -- authorization -----------------------------------------------------

      test "role endpoints are admin-only for every caller type" do
        role = roles(:acme_infra)
        grant = grants(:acme_infra_prod_api_key)
        assert_admin_only_api :get, api_v1_roles_path(limit: 200)
        assert_admin_only_api :get, api_v1_role_path(role.oid)
        assert_admin_only_api :post, api_v1_roles_path, params: { data: { foreign_id: "sneaky" } }
        assert_admin_only_api :patch, api_v1_role_path(role.oid), params: { data: { name: "Sneaky" } }
        assert_admin_only_api :get, api_v1_role_grants_path(role.oid, limit: 200)
        assert_admin_only_api :post, api_v1_grants_path,
                              params: { data: { role_id: role.oid,
                                                static_secret_id: static_secrets(:acme_staging_api_key).oid } }
        assert_admin_only_api :delete, api_v1_grant_path(grant.oid)
        assert_admin_only_api :put, slack_channel_permissions_api_v1_role_path(role.oid),
                              params: { data: { slack_channel_permissions: [] } }
        assert_admin_only_api :delete, slack_channel_permission_api_v1_role_path(role.oid, "C0123456789")
        assert_admin_only_api :get, api_v1_slack_channels_path(exclude_owner: "role:#{role.oid}")

        assert_not Role.exists?(foreign_id: "sneaky")
        assert_equal "Infra", role.reload.name
        assert Grant.exists?(grant.id)
        assert_not role.grants.exists?(static_secret: static_secrets(:acme_staging_api_key))
      end

      test "an unknown role oid is a 404" do
        get api_v1_role_path("rol_missing")
        assert_response :not_found
        get api_v1_role_grants_path("rol_missing")
        assert_response :not_found
        put slack_channel_permissions_api_v1_role_path("rol_missing"),
            params: { data: { slack_channel_permissions: [] } }, headers: csrf_headers, as: :json
        assert_response :not_found
      end

      test "the legacy role page paths are served by the SPA" do
        Dir.mktmpdir do |dir|
          index = File.join(dir, "index.html")
          File.write(index, "<!doctype html><div id=\"root\"></div>")
          previous = SpaController.index_path
          SpaController.index_path = index
          role = roles(:acme_infra)
          [ "/console/roles", "/console/roles/new", "/console/roles/#{role.oid}",
            "/console/roles/#{role.oid}/edit" ].each do |path|
            get path, headers: { "Accept" => "text/html" }
            assert_response :ok, path
            assert_includes response.body, "<div id=\"root\">", path
          end
        ensure
          SpaController.index_path = previous
        end
      end

      # -- index / show ----------------------------------------------------------

      test "index lists every role with identity and labels" do
        get api_v1_roles_path(limit: 200)
        assert_response :ok
        role = json_body["data"].find { |r| r["id"] == roles(:acme_infra).oid }
        assert_equal "acme-infra", role["foreign_id"]
        assert_equal "Infra", role["name"]
        assert_equal({ "kind" => "shared" }, role["labels"])
        assert_equal Role.count, json_body.dig("meta", "total")
      end

      test "show returns the role and its Slack channel permissions" do
        role = roles(:acme_infra)
        role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        get api_v1_role_path(role.oid)
        assert_response :ok
        assert_equal role.name, json_body.dig("data", "name")
        assert_equal [ "C0123456789" ], json_body.dig("data", "slack_channel_permissions").map { |p| p["channel_id"] }
      end

      # -- create / update -------------------------------------------------------

      test "create persists role identity and labels without a namespace" do
        assert_difference -> { Role.count }, 1 do
          post api_v1_roles_path,
               params: { data: { foreign_id: "payments", name: "Payments", labels: { team: "finance" },
                                 namespace: "other" } },
               headers: csrf_headers, as: :json
        end
        assert_response :created

        role = Role.find_by!(foreign_id: "payments")
        assert_equal role.oid, json_body.dig("data", "id")
        assert_equal "Payments", role.name
        assert_equal({ "team" => "finance" }, role.labels)
        assert_equal @operator, role.created_by
        assert_not json_body["data"].key?("namespace")
      end

      test "create accepts a blank foreign id" do
        post api_v1_roles_path, params: { data: { foreign_id: nil, name: "No handle" } },
             headers: csrf_headers, as: :json
        assert_response :created
        assert_nil Role.find_by_oid!(json_body.dig("data", "id")).foreign_id
      end

      test "create rejects an invalid role without writing" do
        assert_no_difference -> { Role.count } do
          post api_v1_roles_path, params: { data: { foreign_id: "bad foreign id", name: "Broken" } },
               headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "foreign_id").present?
      end

      test "update changes mutable fields only" do
        role = roles(:acme_infra)
        patch api_v1_role_path(role.oid),
              params: { data: { foreign_id: "changed", name: "Infrastructure", labels: { kind: "platform" } } },
              headers: csrf_headers, as: :json
        assert_response :ok

        role.reload
        assert_equal "Infrastructure", role.name
        assert_equal "acme-infra", role.foreign_id
        assert_equal({ "kind" => "platform" }, role.labels)
      end

      test "update works over bearer auth" do
        role = roles(:acme_infra)
        patch api_v1_role_path(role.oid), params: { data: { name: "Via bearer" } }, headers: bearer_headers, as: :json
        assert_response :ok
        assert_equal "Via bearer", role.reload.name
      end

      # -- grants ----------------------------------------------------------------

      test "role grants include the secret's kind, name, and foreign id" do
        role = roles(:acme_infra)
        secret = static_secrets(:acme_prod_api_key)

        get api_v1_role_grants_path(role.oid, limit: 200)
        assert_response :ok
        grant = json_body["data"].find { |g| g["id"] == grants(:acme_infra_prod_api_key).oid }
        assert_equal role.oid, grant["role_id"]
        assert_equal({ "kind" => "static", "kind_label" => "Static", "id" => secret.oid,
                       "foreign_id" => secret.foreign_id, "name" => secret.name }, grant["grantable"])
      end

      test "granting a secret uses the default role priority" do
        role = roles(:acme_admin_role)
        secret = static_secrets(:acme_staging_api_key)

        assert_difference -> { role.grants.count }, 1 do
          post api_v1_grants_path, params: { data: { role_id: role.oid, static_secret_id: secret.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :created
        grant = role.grants.find_by!(static_secret: secret)
        assert_equal Grant::DEFAULT_ROLE_PRIORITY, grant.priority
        assert_equal @operator, grant.created_by
        assert_equal secret.name, json_body.dig("data", "grantable", "name")
      end

      test "granting a secret to a role is idempotent" do
        role = roles(:acme_infra)
        secret = static_secrets(:acme_prod_api_key)

        assert_no_difference -> { role.grants.count } do
          post api_v1_grants_path, params: { data: { role_id: role.oid, static_secret_id: secret.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :ok
        assert_equal grants(:acme_infra_prod_api_key).oid, json_body.dig("data", "id")
      end

      test "granting allows any secret" do
        role = roles(:acme_admin_role)
        secret = static_secrets(:globex_prod_secret)

        assert_difference -> { Grant.count } => 1 do
          post api_v1_grants_path, params: { data: { role_id: role.oid, static_secret_id: secret.oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :created
        assert_equal "prod-secret", json_body.dig("data", "grantable", "name")
      end

      test "granting without a secret is a validation error" do
        assert_no_difference -> { Grant.count } do
          post api_v1_grants_path, params: { data: { role_id: roles(:acme_infra).oid } },
               headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
      end

      test "revoking removes the role grant" do
        role = roles(:acme_infra)
        grant = grants(:acme_infra_prod_api_key)

        assert_difference -> { role.grants.count }, -1 do
          delete api_v1_grant_path(grant.oid), headers: csrf_headers
        end
        assert_response :no_content
        assert_not Grant.exists?(grant.id)
      end

      # -- Slack channel permissions -------------------------------------------

      test "replacing stores role permissions" do
        role = roles(:acme_infra)
        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: true, download_enabled: false, history_enabled: true }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok

        permission = role.slack_channel_permissions.reload.sole
        assert_equal "C0123456789", permission.channel_id
        assert_predicate permission, :upload_enabled
        assert_not permission.download_enabled
        assert_predicate permission, :history_enabled
        assert_equal [ permission.as_permission_json ], json_body["data"]
      end

      test "replacing updates the flags of an existing channel" do
        role = roles(:acme_infra)
        role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)

        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: false, download_enabled: true, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok

        permission = role.slack_channel_permissions.reload.sole
        assert_equal "C0123456789", permission.channel_id
        assert_not permission.upload_enabled
        assert_predicate permission, :download_enabled
        assert_not permission.history_enabled
      end

      test "rows are keyed by channel id, so a submitted record id cannot rename a channel" do
        role = roles(:acme_infra)
        permission = role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)

        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { id: permission.id, channel_id: "C0123456789", upload_enabled: true },
              { id: permission.id, channel_id: "G9876543210", upload_enabled: true }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal %w[C0123456789 G9876543210], role.slack_channel_permissions.reload.pluck(:channel_id).sort
      end

      test "replacing bumps the sync config versions of the role's principals" do
        role = roles(:acme_infra)
        role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        role.slack_channel_permissions.create!(channel_id: "G9876543210", download_enabled: true)
        versions = Principal.where(id: role.principal_ids).pluck(:id, :sync_config_cache_version).to_h
        assert_not_empty versions

        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: false, download_enabled: true, history_enabled: false },
              { channel_id: "C2222222222", upload_enabled: true, download_enabled: false, history_enabled: true }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok

        assert_equal %w[C0123456789 C2222222222], role.slack_channel_permissions.reload.pluck(:channel_id).sort
        Principal.where(id: role.principal_ids).find_each do |principal|
          assert_equal versions.fetch(principal.id) + 1, principal.sync_config_cache_version
        end
      end

      test "an unchanged replacement writes nothing" do
        role = roles(:acme_infra)
        permission = role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        versions = Principal.where(id: role.principal_ids).pluck(:id, :sync_config_cache_version).to_h

        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: true, download_enabled: false, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok

        assert_equal [ permission.id ], role.slack_channel_permissions.reload.pluck(:id)
        Principal.where(id: role.principal_ids).find_each do |principal|
          assert_equal versions.fetch(principal.id), principal.sync_config_cache_version
        end
      end

      test "roles keep D channel rows because they have no API-managed DMs" do
        role = roles(:acme_infra)
        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "D0123456789", upload_enabled: true, download_enabled: false, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal [ "D0123456789" ], role.slack_channel_permissions.reload.pluck(:channel_id)
      end

      test "replacing rejects rows without any permission" do
        role = roles(:acme_infra)
        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [
              { channel_id: "C0123456789", upload_enabled: false, download_enabled: false, history_enabled: false }
            ] } },
            headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_match(/Select at least one Slack permission/, json_body.dig("error", "details").to_s)
        assert_empty role.slack_channel_permissions.reload
      end

      test "a single role Slack channel permission can be deleted" do
        role = roles(:acme_infra)
        delete_permission = role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
        keep_permission = role.slack_channel_permissions.create!(channel_id: "G9876543210", upload_enabled: true)

        assert_difference -> { role.slack_channel_permissions.count }, -1 do
          delete slack_channel_permission_api_v1_role_path(role.oid, "c0123456789"), headers: csrf_headers
        end
        assert_response :no_content
        assert_raises(ActiveRecord::RecordNotFound) { delete_permission.reload }
        assert_predicate keep_permission.reload, :persisted?

        delete slack_channel_permission_api_v1_role_path(role.oid, "C0123456789"), headers: csrf_headers
        assert_response :not_found
      end

      test "deleting another role's channel is a 404" do
        other = roles(:acme_admin_role)
        permission = other.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)

        delete slack_channel_permission_api_v1_role_path(roles(:acme_infra).oid, "C0123456789"), headers: csrf_headers
        assert_response :not_found
        assert_predicate permission.reload, :persisted?
      end

      test "Slack channel permission routes work over bearer auth" do
        role = roles(:acme_infra)
        put slack_channel_permissions_api_v1_role_path(role.oid),
            params: { data: { slack_channel_permissions: [ { channel_id: "C0123456789", history_enabled: true } ] } },
            headers: bearer_headers, as: :json
        assert_response :ok
        delete slack_channel_permission_api_v1_role_path(role.oid, "C0123456789"), headers: bearer_headers
        assert_response :no_content
      end

      # -- Slack channel catalog (role owner) ------------------------------------

      test "role channel options use the shared catalog lookup and exclude the role's channels" do
        role = roles(:acme_infra)
        role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)

        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token") do
          get api_v1_slack_channels_path(q: "no-match", exclude_owner: "role:#{role.oid}")
          assert_response :ok
          assert_equal [], json_body["data"]

          get api_v1_slack_channels_path(exclude_owner: "role:#{role.oid}")
        end
        assert_not_includes json_body["data"].map { |c| c["channel_id"] }, "C0123456789"
        assert_includes json_body["data"].map { |c| c["channel_id"] }, "G9876543210"
      end

      test "role channel options 404 for an unknown role" do
        get api_v1_slack_channels_path(exclude_owner: "role:rol_missing")
        assert_response :not_found
      end
    end
  end
end
