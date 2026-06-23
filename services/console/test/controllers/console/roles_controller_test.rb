require "test_helper"

module Console
  class RolesControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "redirects to login when signed out" do
      delete logout_url
      get console_roles_url
      assert_redirected_to login_path
    end

    test "index lists roles and links to new" do
      role = roles(:acme_infra)
      get console_roles_url
      assert_response :ok
      assert_select "h1", text: "Roles"
      assert_select "a[href=?]", new_console_role_path, text: "Add Role"
      assert_select "tr[onclick=?]", "window.location='#{console_role_path(role.oid)}'"
    end

    test "show renders role details and editable secret grants" do
      role = roles(:acme_infra)
      grant = grants(:acme_infra_prod_api_key)
      get console_role_url(role.oid)
      assert_response :ok
      assert_select "h1", text: role.name
      assert_select "a[href=?]", edit_console_role_path(role.oid), text: "Edit"
      assert_select "a[href=?]", console_secret_path("static", static_secrets(:acme_prod_api_key).oid)
      assert_select "form[action=?]", grant_secret_console_role_path(role.oid) do
        assert_select "select[name=grantable][aria-label=?]", "Secret to grant"
        assert_select "option[value=?]", "static:#{static_secrets(:acme_staging_api_key).oid}"
        assert_select "option[value=?]", "static:#{static_secrets(:acme_prod_api_key).oid}", count: 0
        assert_select "option[value=?]", "static:#{static_secrets(:globex_prod_secret).oid}", count: 0
      end
      assert_select "form[action=?]", revoke_grant_console_role_path(role.oid, grant.oid) do
        assert_select "button[type=submit]", "Revoke"
      end
    end

    test "new and edit render forms" do
      role = roles(:acme_infra)
      get new_console_role_url
      assert_response :ok
      assert_select "form[action=?]", console_roles_path
      assert_select "input[name=?]", "role[namespace]"
      assert_select "input[name=?]", "role[foreign_id]"

      get edit_console_role_url(role.oid)
      assert_response :ok
      assert_select "form[action=?]", console_role_path(role.oid)
      assert_select "input[name=?]", "role[name]"
      assert_select "input[name=?]", "role[namespace]", count: 0
      assert_select "input[name=?]", "role[foreign_id]", count: 0
    end

    test "create persists role identity and labels" do
      assert_difference -> { Role.count }, 1 do
        post console_roles_url, params: {
          role: { namespace: "acme", foreign_id: "payments", name: "Payments" },
          labels: { "0" => { key: "team", value: "finance" } }
        }
      end

      role = Role.find_by!(namespace: "acme", foreign_id: "payments")
      assert_redirected_to console_role_path(role.oid)
      assert_equal "Payments", role.name
      assert_equal({ "team" => "finance" }, role.labels)
    end

    test "create rejects invalid role without writing" do
      assert_no_difference -> { Role.count } do
        post console_roles_url, params: {
          role: { namespace: "bad namespace", foreign_id: "broken", name: "Broken" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "update changes mutable fields only" do
      role = roles(:acme_infra)
      patch console_role_url(role.oid), params: {
        role: { namespace: "globex", foreign_id: "changed", name: "Infrastructure" },
        labels: { "0" => { key: "kind", value: "platform" } }
      }

      assert_redirected_to console_role_path(role.oid)
      role.reload
      assert_equal "Infrastructure", role.name
      assert_equal "acme", role.namespace
      assert_equal "infra", role.foreign_id
      assert_equal({ "kind" => "platform" }, role.labels)
    end

    test "grant_secret grants a same-namespace secret at the default role priority" do
      role = roles(:acme_admin_role)
      secret = static_secrets(:acme_staging_api_key)

      assert_difference -> { role.grants.count }, 1 do
        post grant_secret_console_role_url(role.oid), params: { grantable: "static:#{secret.oid}" }
      end

      assert_redirected_to console_role_path(role.oid)
      grant = role.grants.find_by(static_secret: secret)
      assert_not_nil grant
      assert_equal Grant::DEFAULT_ROLE_PRIORITY, grant.priority
    end

    test "grant_secret is idempotent" do
      role = roles(:acme_infra)
      secret = static_secrets(:acme_prod_api_key)

      assert_no_difference -> { role.grants.count } do
        post grant_secret_console_role_url(role.oid), params: { grantable: "static:#{secret.oid}" }
      end

      assert_redirected_to console_role_path(role.oid)
    end

    test "grant_secret rejects a secret from another namespace" do
      role = roles(:acme_admin_role)
      secret = static_secrets(:globex_prod_secret)

      assert_no_difference -> { Grant.count } do
        post grant_secret_console_role_url(role.oid), params: { grantable: "static:#{secret.oid}" }
      end

      assert_redirected_to console_role_path(role.oid)
      assert_equal "Secret must be in the same namespace as the role.", flash[:alert]
    end

    test "revoke_grant removes the role grant" do
      role = roles(:acme_infra)
      grant = grants(:acme_infra_prod_api_key)

      assert_difference -> { role.grants.count }, -1 do
        delete revoke_grant_console_role_url(role.oid, grant.oid)
      end

      assert_redirected_to console_role_path(role.oid)
      assert_not Grant.exists?(grant.id)
    end

    test "unknown role returns 404" do
      get console_role_url("role_missing")
      assert_response :not_found
    end
  end
end
