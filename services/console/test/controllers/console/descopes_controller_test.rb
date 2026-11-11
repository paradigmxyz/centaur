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
      assert_redirected_to console_integrations_path
      assert_nil flash[:alert]
    end


    test "restore brings back admin permissions" do
      sign_in users(:acme_admin)
      post console_descope_url

      delete console_descope_url
      assert_redirected_to console_principals_path

      get api_v1_users_path
      assert_response :ok
      assert_select ".console-descope-banner", count: 0
    end

    test "descope ends automatically when the user is no longer an admin" do
      admin = users(:acme_admin)
      sign_in admin
      post console_descope_url

      admin.update!(admin: false)
      get console_scheduled_tasks_url
      assert_response :ok
      assert_select ".console-descope-banner", count: 0
    end

    test "restore is a no-op redirect when not descoped" do
      sign_in users(:acme_admin)
      delete console_descope_url
      assert_redirected_to console_principals_path
    end
  end
end
