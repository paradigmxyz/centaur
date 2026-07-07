require "test_helper"

module Console
  # Covers admin self-descope ("view as operator"): who can start it, that admin
  # gates and admin chrome disappear while descoped, how it's restored, and the
  # self-healing session cleanup when the user is no longer an admin.
  class DescopesControllerTest < ActionDispatch::IntegrationTest
    def sign_in(user)
      post login_url, params: { email: user.email, password: "password123456" }
    end

    test "a non-admin cannot descope" do
      sign_in users(:member_user)
      post console_descope_url
      assert_redirected_to console_threads_path
      assert_nil flash[:alert]
    end

    test "a descoped admin loses admin pages and chrome, and sees the banner" do
      sign_in users(:acme_admin)
      post console_descope_url
      assert_redirected_to console_threads_path

      get console_users_url
      assert_redirected_to console_threads_path
      assert_nil flash[:alert]

      get console_threads_url
      assert_response :ok
      assert_select ".console-descope-banner", /Admin permissions paused/
      assert_select ".console-nav-link", text: "Control", count: 0
      assert_select "form[action=?]", console_descope_path do
        assert_select "button", text: /Restore admin/
      end
    end

    test "restore brings back admin permissions" do
      sign_in users(:acme_admin)
      post console_descope_url

      delete console_descope_url
      assert_redirected_to console_principals_path

      get console_users_url
      assert_response :ok
      assert_select ".console-descope-banner", count: 0
    end

    test "descope ends automatically when the user is no longer an admin" do
      admin = users(:acme_admin)
      sign_in admin
      post console_descope_url

      admin.update!(admin: false)
      get console_threads_url
      assert_response :ok
      assert_select ".console-descope-banner", count: 0
    end

    test "restore is a no-op redirect when not descoped" do
      sign_in users(:acme_admin)
      delete console_descope_url
      assert_redirected_to console_principals_path
    end

    test "the account menu offers descope only to acting admins" do
      sign_in users(:acme_admin)
      get console_threads_url
      assert_select ".console-signout-label", text: "View as operator"

      sign_in users(:member_user)
      get console_threads_url
      assert_select ".console-signout-label", text: "View as operator", count: 0
    end
  end
end
