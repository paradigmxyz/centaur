require "test_helper"

module Console
  class SystemSettingsControllerTest < ActionDispatch::IntegrationTest
    def sign_in(user)
      post login_url, params: { email: user.email, password: "password123456" }
    end

    test "redirects to login when signed out" do
      get edit_console_system_settings_url
      assert_redirected_to login_path
    end

    test "non-admin users cannot edit settings" do
      sign_in users(:member_user)
      get edit_console_system_settings_url
      assert_redirected_to console_threads_path
    end

    test "admin can edit system settings" do
      sign_in users(:acme_admin)

      get edit_console_system_settings_url
      assert_response :ok

      assert_select ".console-control-tab-active", text: "Settings"
      assert_select "select[name='system_setting[default_sandbox_repo_cache]']"
      assert_select "input[name='system_setting[default_sandbox_observability_enabled]']"
      assert_select "input[name='system_setting[default_sandbox_api_server_enabled]']"
    end

    test "admin updates default sandbox capabilities" do
      sign_in users(:acme_admin)

      patch console_system_settings_url,
            params: {
              system_setting: {
                default_sandbox_repo_cache: "public",
                default_sandbox_observability_enabled: "0",
                default_sandbox_api_server_enabled: "0"
              }
            }

      assert_redirected_to edit_console_system_settings_path
      assert_equal "System settings updated.", flash[:notice]
      settings = system_settings(:default).reload
      assert_equal "public", settings.default_sandbox_repo_cache
      assert_equal false, settings.default_sandbox_observability_enabled
      assert_equal false, settings.default_sandbox_api_server_enabled
    end
  end
end
