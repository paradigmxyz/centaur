require "test_helper"

module Console
  # Covers the role-assignment and direct-grant mutations wired from the principal
  # detail page: assign/unassign roles and grant/revoke secrets, plus idempotency
  # and the signed-out gate.
  class PrincipalsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "redirects to login when signed out" do
      delete logout_url
      post console_principal_assign_role_url(principals(:acme_user_bob).oid),
           params: { role_id: roles(:acme_admin_role).oid }
      assert_redirected_to login_path
    end

    test "new renders the create form" do
      get console_new_principal_url
      assert_response :ok
      assert_select "form[action=?][method=?]", console_create_principal_path, "post" do
        assert_select "input[name='principal[namespace]'][value=default]"
        assert_select "input[name='principal[foreign_id]']"
        assert_select "input[name='principal[name]']"
        assert_select "button", "Add label"
        assert_select "input[type=submit][value='Add Principal']"
      end
    end

    test "create persists a principal and redirects to its detail page" do
      assert_difference -> { Principal.count }, 1 do
        post console_create_principal_url,
             params: {
               principal: { namespace: "acme", foreign_id: "C-new-console", name: "New console principal" },
               labels: {
                 "0" => { key: "kind", value: "slack_channel" },
                 "1" => { key: "team", value: "platform" }
               }
             }
      end

      principal = Principal.find_by!(namespace: "acme", foreign_id: "C-new-console")
      assert_redirected_to console_principal_path(principal.oid)
      assert_equal "Principal created.", flash[:notice]
      assert_equal "New console principal", principal.name
      assert_equal({ "kind" => "slack_channel", "team" => "platform" }, principal.labels)
      assert_equal @operator, principal.created_by
    end

    test "create re-renders validation errors" do
      existing = principals(:acme_channel)

      assert_no_difference -> { Principal.count } do
        post console_create_principal_url,
             params: {
               principal: { namespace: existing.namespace, foreign_id: existing.foreign_id, name: "Duplicate" }
             }
      end

      assert_response :unprocessable_entity
      assert_select ".alert-error", text: /Principal could not be saved/
      assert_select ".field-error", text: /has already been taken/
    end

    test "update_sandbox_access toggles sandbox capabilities" do
      principal = principals(:acme_user_bob)

      patch console_principal_sandbox_access_url(principal.oid),
            params: {
              sandbox_repo_cache: "public",
              sandbox_observability_enabled: "0",
              sandbox_api_server_enabled: "0"
            }

      assert_redirected_to console_principal_path(principal.oid)
      assert_equal "Updated sandbox access.", flash[:notice]
      principal.reload
      assert_equal "public", principal.sandbox_repo_cache
      assert_equal false, principal.sandbox_repo_cache_enabled
      assert_equal false, principal.sandbox_observability_enabled
      assert_equal false, principal.sandbox_api_server_enabled
    end

    test "update_slack_channel_permissions stores selected Slack channel permissions" do
      principal = principals(:acme_user_bob)

      patch console_principal_slack_channel_permissions_url(principal.oid),
            params: {
              principal: {
                slack_channel_permissions_attributes: {
                  "0" => {
                    channel_id: "C0123456789",
                    upload_enabled: "1",
                    download_enabled: "0",
                    history_enabled: "1"
                  },
                  "1" => {
                    channel_id: "G9876543210",
                    upload_enabled: "0",
                    download_enabled: "1",
                    history_enabled: "0"
                  }
                }
              }
            }

      assert_redirected_to console_principal_path(principal.oid)
      assert_equal(
        [
          {
            "channel_id" => "C0123456789",
            "channel_name" => nil,
            "upload_enabled" => true,
            "download_enabled" => false,
            "history_enabled" => true
          },
          {
            "channel_id" => "G9876543210",
            "channel_name" => nil,
            "upload_enabled" => false,
            "download_enabled" => true,
            "history_enabled" => false
          }
        ],
        principal.reload.slack_channel_permissions_payload
      )
    end

    test "update_slack_channel_permissions clears stale channel names when changing channels" do
      principal = principals(:acme_user_bob)
      permission = SlackChannelPermission.create!(
        principal: principal,
        channel_id: "C0123456789",
        channel_name: "old-channel",
        upload_enabled: true,
        download_enabled: true,
        history_enabled: true
      )

      patch console_principal_slack_channel_permissions_url(principal.oid),
            params: {
              principal: {
                slack_channel_permissions_attributes: {
                  "0" => {
                    id: permission.id,
                    channel_id: "G9876543210",
                    channel_name: "",
                    upload_enabled: "1",
                    download_enabled: "1",
                    history_enabled: "1"
                  }
                }
              }
            }

      assert_redirected_to console_principal_path(principal.oid)
      permission.reload
      assert_equal "G9876543210", permission.channel_id
      assert_nil permission.channel_name
    end

    test "destroy deletes the principal and dependent access records" do
      principal = principals(:acme_channel)
      proxy = proxies(:acme_proxy)
      client = McpOauthClient.create!(redirect_uris: [ "http://localhost/callback" ])
      McpOauthAuthorizationCode.create!(
        mcp_oauth_client: client,
        user: users(:acme_admin),
        principal: principal,
        redirect_uri: "http://localhost/callback",
        code_challenge: "challenge",
        resource: "https://api.example.test",
        scopes: %w[mcp:tools]
      )
      McpOauthRefreshToken.create!(
        mcp_oauth_client: client,
        user: users(:acme_admin),
        principal: principal,
        resource: "https://api.example.test",
        scopes: %w[mcp:tools]
      )

      assert_difference -> { Principal.count }, -1 do
        assert_difference -> { Grant.where(principal: principal).count }, -3 do
          assert_difference -> { PrincipalRole.where(principal: principal).count }, -1 do
            assert_difference -> { McpOauthAuthorizationCode.where(principal: principal).count }, -1 do
              assert_difference -> { McpOauthRefreshToken.where(principal: principal).count }, -1 do
                delete console_delete_principal_url(principal.oid)
              end
            end
          end
        end
      end

      assert_redirected_to console_principals_path
      assert_equal "Deleted principal #{principal.foreign_id}.", flash[:notice]
      assert_nil proxy.reload.principal
    end

    test "assign_role attaches the role and redirects with a notice" do
      principal = principals(:acme_user_bob)
      role = roles(:acme_admin_role)
      assert_difference -> { principal.principal_roles.count }, 1 do
        post console_principal_assign_role_url(principal.oid), params: { role_id: role.oid }
      end
      assert_redirected_to console_principal_path(principal.oid)
      assert_equal "Assigned role #{role.name}.", flash[:notice]
      assert principal.reload.roles.include?(role)
    end

    test "assign_role is idempotent" do
      principal = principals(:acme_user_alice)
      role = roles(:acme_infra) # already assigned via fixture
      assert_no_difference -> { principal.principal_roles.count } do
        post console_principal_assign_role_url(principal.oid), params: { role_id: role.oid }
      end
      assert_redirected_to console_principal_path(principal.oid)
    end

    test "unassign_role detaches the role" do
      principal = principals(:acme_user_alice)
      role = roles(:acme_infra)
      assert_difference -> { principal.principal_roles.count }, -1 do
        delete console_principal_unassign_role_url(principal.oid, role.oid)
      end
      assert_redirected_to console_principal_path(principal.oid)
      assert_not principal.reload.roles.include?(role)
    end

    test "grant_secret creates a direct grant at the default direct priority" do
      principal = principals(:acme_user_bob)
      secret = static_secrets(:github_token_inject)
      assert_difference -> { principal.grants.count }, 1 do
        post console_principal_grant_secret_url(principal.oid), params: { grantable: "static:#{secret.oid}" }
      end
      assert_redirected_to console_principal_path(principal.oid)
      grant = principal.grants.find_by(static_secret: secret)
      assert_not_nil grant
      assert_equal Grant::DEFAULT_DIRECT_PRIORITY, grant.priority
    end

    test "grant_secret works for a non-static secret kind" do
      principal = principals(:acme_user_bob)
      secret = pg_dsn_secrets(:acme_analytics_pg)
      assert_difference -> { principal.grants.count }, 1 do
        post console_principal_grant_secret_url(principal.oid), params: { grantable: "pg_dsn:#{secret.oid}" }
      end
      assert principal.grants.exists?(pg_dsn_secret: secret)
    end

    test "grant_secret is idempotent" do
      principal = principals(:acme_channel)
      secret = static_secrets(:github_token_inject) # already granted via fixture
      assert_no_difference -> { principal.grants.count } do
        post console_principal_grant_secret_url(principal.oid), params: { grantable: "static:#{secret.oid}" }
      end
      assert_redirected_to console_principal_path(principal.oid)
    end

    test "grant_secret with a blank selection flashes an alert" do
      principal = principals(:acme_user_bob)
      assert_no_difference -> { principal.grants.count } do
        post console_principal_grant_secret_url(principal.oid), params: { grantable: "" }
      end
      assert_equal "Pick a secret to grant.", flash[:alert]
    end

    test "revoke_grant removes the direct grant" do
      principal = principals(:acme_channel)
      grant = grants(:acme_channel_github_token)
      assert_difference -> { principal.grants.count }, -1 do
        delete console_principal_revoke_grant_url(principal.oid, grant.oid)
      end
      assert_redirected_to console_principal_path(principal.oid)
      assert_not Grant.exists?(grant.id)
    end

    test "an unknown principal oid is a 404" do
      post console_principal_assign_role_url("prn_missing"), params: { role_id: roles(:acme_infra).oid }
      assert_response :not_found
    end
  end
end
