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

    test "update_sandbox_access toggles repo cache and observability access" do
      principal = principals(:acme_user_bob)

      patch console_principal_sandbox_access_url(principal.oid),
            params: {
              sandbox_repo_cache_enabled: "0",
              sandbox_observability_enabled: "0"
            }

      assert_redirected_to console_principal_path(principal.oid)
      assert_equal "Updated sandbox access.", flash[:notice]
      principal.reload
      assert_equal false, principal.sandbox_repo_cache_enabled
      assert_equal false, principal.sandbox_observability_enabled
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
