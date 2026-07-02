require "test_helper"

module Console
  # Covers the admin-only operator management screen: who can reach it, and the
  # approve/disable/promote state transitions (including the self-disable guard).
  class UsersControllerTest < ActionDispatch::IntegrationTest
    def sign_in(user)
      post login_url, params: { email: user.email, password: "password123456" }
    end

    test "redirects to login when signed out" do
      get console_users_url
      assert_redirected_to login_path
    end

    test "an active non-admin is forbidden" do
      sign_in users(:member_user)
      get console_users_url
      assert_redirected_to console_threads_path
      assert_equal "That page is restricted to admins.", flash[:alert]
    end

    test "an admin sees the index with pending users listed" do
      sign_in users(:acme_admin)
      get console_users_url
      assert_response :ok
      assert_select "td", /pending@acme.example/
      assert_select ".console-nav-link", text: "Control"
      assert_select ".console-nav-link", text: "Apps", count: 0
      assert_select ".console-nav-link", text: "Users", count: 0
      assert_select ".console-control-tab", text: "Apps"
      assert_select ".console-control-tab-active", text: "Users"
      assert_select "button[data-console-theme-toggle]", text: "Light mode"
      assert_select "link[data-console-favicon][href=?]", "/icon-dark.svg"
      assert_includes response.body, "/icon-light.svg"
      assert_includes response.body, "prefers-color-scheme: light"
      assert_includes response.body, "centaur-console-theme-source"
    end

    test "the index shows IdP chips for linked identities and a password chip otherwise" do
      sign_in users(:acme_admin)
      get console_users_url
      assert_select "span", text: "Google"   # acme_admin is linked via Google
      assert_select "span", text: "Slack"    # pending_user is linked via Slack
      assert_select "span", text: "Password" # member_user has no linked identity
    end

    test "approve activates a pending user and records the approver" do
      admin = users(:acme_admin)
      sign_in admin
      pending = users(:pending_user)
      post approve_console_user_url(pending.oid)
      assert_redirected_to console_users_path
      assert pending.reload.active?
      assert_equal admin, pending.approved_by
    end

    test "disable revokes an active user" do
      sign_in users(:acme_admin)
      target = users(:member_user)
      post disable_console_user_url(target.oid)
      assert_redirected_to console_users_path
      assert target.reload.disabled?
    end

    test "disable revokes outstanding MCP OAuth refresh tokens" do
      sign_in users(:acme_admin)
      target = users(:member_user)
      refresh = McpOauthRefreshToken.create!(
        mcp_oauth_client: McpOauthClient.create!(
          name: "Amp",
          redirect_uris: [ "http://127.0.0.1:49152/callback" ],
          grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
          response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
          scopes: McpOauthClient::DEFAULT_SCOPES
        ),
        user: target,
        principal: principals(:acme_channel),
        resource: "http://localhost:3000/mcp",
        scopes: [ "mcp:tools" ]
      )

      post disable_console_user_url(target.oid)

      assert_redirected_to console_users_path
      assert target.reload.disabled?
      assert refresh.reload.revoked_at.present?
    end

    test "an admin cannot disable their own account" do
      admin = users(:acme_admin)
      sign_in admin
      post disable_console_user_url(admin.oid)
      assert_redirected_to console_users_path
      assert_equal "You can't disable your own account.", flash[:alert]
      assert admin.reload.active?
    end

    test "promote makes a user an active admin" do
      sign_in users(:acme_admin)
      target = users(:pending_user)
      post promote_console_user_url(target.oid)
      assert_redirected_to console_users_path
      target.reload
      assert target.admin?
      assert target.active?
    end

    test "a non-admin cannot perform actions" do
      sign_in users(:member_user)
      target = users(:pending_user)
      post approve_console_user_url(target.oid)
      assert_redirected_to console_threads_path
      assert target.reload.pending?
    end
  end
end
