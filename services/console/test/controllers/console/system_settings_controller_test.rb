require "test_helper"

module Console
  class SystemSettingsControllerTest < ActionDispatch::IntegrationTest
    class FakeApiClient
      attr_reader :replaced

      def initialize(team_ids: [], error: nil)
        @team_ids = team_ids
        @error = error
      end

      def slack_external_team_ids
        raise @error if @error

        @team_ids
      end

      def replace_slack_external_team_ids(team_ids)
        raise @error if @error

        @replaced = team_ids
      end
    end

    setup do
      @original_api_client_factory = SystemSettingsController.api_client_factory
      use_api_client(FakeApiClient.new)
    end

    teardown do
      SystemSettingsController.api_client_factory = @original_api_client_factory
    end

    def use_api_client(client)
      SystemSettingsController.api_client_factory = -> { client }
    end

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
      assert_redirected_to console_integrations_path
    end

    test "admin can edit system settings" do
      sign_in users(:acme_admin)

      get edit_console_system_settings_url
      assert_response :ok

      assert_select ".console-control-tab-active", text: "Settings"
      assert_select "select[name='system_setting[default_sandbox_repo_cache]']"
      assert_select "input[name='system_setting[default_sandbox_observability_enabled]']"
      assert_select "input[name='system_setting[default_sandbox_sessions_read_enabled]']"
      assert_select "input[name='system_setting[default_sandbox_workflows_read_enabled]']"
      assert_select "input[name='system_setting[default_sandbox_workflows_write_enabled]']"
      assert_select "input[name='system_setting[default_role_ids][]'][value=?]", roles(:acme_infra).id.to_s
    end

    test "admin updates principal defaults" do
      sign_in users(:acme_admin)

      patch console_system_settings_url,
            params: {
              system_setting: {
                default_sandbox_repo_cache: "public",
                default_sandbox_observability_enabled: "0",
                default_sandbox_sessions_read_enabled: "0",
                default_sandbox_workflows_read_enabled: "0",
                default_sandbox_workflows_write_enabled: "0",
                default_role_ids: [ roles(:acme_infra).id, roles(:globex_infra).id ]
              }
            }

      assert_redirected_to edit_console_system_settings_path
      assert_equal "System settings updated.", flash[:notice]
      settings = system_settings(:default).reload
      assert_equal "public", settings.default_sandbox_repo_cache
      assert_equal false, settings.default_sandbox_observability_enabled
      assert_equal false, settings.default_sandbox_sessions_read_enabled
      assert_equal false, settings.default_sandbox_workflows_read_enabled
      assert_equal false, settings.default_sandbox_workflows_write_enabled
      assert_predicate roles(:acme_infra).reload, :assign_by_default?
      assert_predicate roles(:globex_infra).reload, :assign_by_default?
      assert_not roles(:default_infra).reload.assign_by_default?
    end

    test "admin can clear all default roles" do
      sign_in users(:acme_admin)
      roles(:acme_infra).update!(assign_by_default: true)

      patch console_system_settings_url,
            params: { system_setting: { default_role_ids: [ "" ] } }

      assert_redirected_to edit_console_system_settings_path
      assert_empty Role.where(assign_by_default: true)
    end

    test "updating sandbox defaults without role params preserves default roles" do
      sign_in users(:acme_admin)
      role = roles(:acme_infra)
      role.update!(assign_by_default: true)

      patch console_system_settings_url,
            params: { system_setting: { default_sandbox_repo_cache: "public" } }

      assert_redirected_to edit_console_system_settings_path
      assert_predicate role.reload, :assign_by_default?
    end

    test "admin sees the allowed Slack Connect workspaces" do
      use_api_client(FakeApiClient.new(team_ids: %w[T0PARTNER1 T0PARTNER2]))
      sign_in users(:acme_admin)

      get edit_console_system_settings_url

      assert_response :ok
      assert_select "textarea[name='slack_external_team_ids']", text: "T0PARTNER1\nT0PARTNER2"
    end

    test "hides the Slack Connect form when the API is unavailable" do
      use_api_client(FakeApiClient.new(error: Errno::ECONNREFUSED.new))
      sign_in users(:acme_admin)

      get edit_console_system_settings_url

      assert_response :ok
      assert_select "textarea[name='slack_external_team_ids']", count: 0
      assert_select "#slack-connect", text: /could not be loaded/
    end

    test "admin replaces the allowed Slack Connect workspaces" do
      client = FakeApiClient.new
      use_api_client(client)
      sign_in users(:acme_admin)

      patch slack_external_teams_console_system_settings_url,
            params: { slack_external_team_ids: "T0PARTNER1, t0partner2\nT0PARTNER3" }

      assert_redirected_to edit_console_system_settings_path
      assert_equal %w[T0PARTNER1 t0partner2 T0PARTNER3], client.replaced
    end

    test "shows Slack team id validation errors from the API" do
      use_api_client(FakeApiClient.new(error: CentaurApiClient::Error.new("invalid Slack team ids: C0CHANNEL", status: 400)))
      sign_in users(:acme_admin)

      patch slack_external_teams_console_system_settings_url,
            params: { slack_external_team_ids: "T0PARTNER1 C0CHANNEL" }

      assert_response :unprocessable_entity
      assert_select "#slack-connect .alert-error", text: "invalid Slack team ids: C0CHANNEL"
      assert_select "textarea[name='slack_external_team_ids']", text: "T0PARTNER1 C0CHANNEL"
    end

    test "non-admin users cannot change Slack Connect workspaces" do
      client = FakeApiClient.new
      use_api_client(client)
      sign_in users(:member_user)

      patch slack_external_teams_console_system_settings_url, params: { slack_external_team_ids: "T0PARTNER1" }

      assert_redirected_to console_integrations_path
      assert_nil client.replaced
    end
  end
end
