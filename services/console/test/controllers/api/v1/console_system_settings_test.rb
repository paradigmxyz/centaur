require "test_helper"

module Api
  module V1
    class ConsoleSystemSettingsTest < ActionDispatch::IntegrationTest
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
        sign_in_console(users(:acme_admin))
      end

      teardown do
        SystemSettingsController.api_client_factory = @original_api_client_factory
      end

      def use_api_client(client)
        SystemSettingsController.api_client_factory = -> { client }
      end

      def patch_settings(data)
        patch api_v1_system_settings_path, params: { data: data }, headers: csrf_headers, as: :json
      end

      test "settings endpoints are admin-only for every caller type" do
        client = FakeApiClient.new
        use_api_client(client)
        before = system_settings(:default).reload.default_sandbox_repo_cache

        assert_admin_only_api :get, api_v1_system_settings_path
        assert_admin_only_api :patch, api_v1_system_settings_path, params: { data: { default_sandbox_repo_cache: "all" } }
        assert_admin_only_api :get, slack_external_teams_api_v1_system_settings_path
        assert_admin_only_api :put, slack_external_teams_api_v1_system_settings_path,
                              params: { data: { slack_external_team_ids: [ "T0PARTNER1" ] } }

        assert_equal before, system_settings(:default).reload.default_sandbox_repo_cache
        assert_nil client.replaced
      end

      test "a signed-out request is refused" do
        reset!
        get api_v1_system_settings_path
        assert_response :unauthorized
      end

      test "non-admin users cannot read or change settings" do
        client = FakeApiClient.new
        use_api_client(client)
        sign_in_console(users(:member_user))

        get api_v1_system_settings_path
        assert_response :forbidden
        put slack_external_teams_api_v1_system_settings_path, params: { data: { slack_external_team_ids: "T0PARTNER1" } },
            headers: csrf_headers, as: :json
        assert_response :forbidden
        assert_nil client.replaced
      end

      test "admin reads system settings with every role's default flag" do
        roles(:acme_infra).update!(assign_by_default: true)

        get api_v1_system_settings_path
        assert_response :ok
        data = json_body["data"]
        settings = system_settings(:default).reload
        assert_equal settings.default_sandbox_repo_cache, data["default_sandbox_repo_cache"]
        %w[observability_enabled sessions_read_enabled workflows_read_enabled workflows_write_enabled].each do |flag|
          assert_equal settings.public_send(:"default_sandbox_#{flag}"), data["default_sandbox_#{flag}"], flag
        end
        assert_includes data["default_role_ids"], roles(:acme_infra).oid
        role = data["roles"].find { |r| r["id"] == roles(:acme_infra).oid }
        assert_equal({ "id" => roles(:acme_infra).oid, "foreign_id" => "acme-infra", "name" => "Infra", "assign_by_default" => true }, role)
        assert_equal Role.count, data["roles"].size
      end

      test "admin updates principal defaults" do
        patch_settings(
          default_sandbox_repo_cache: "public",
          default_sandbox_observability_enabled: false,
          default_sandbox_sessions_read_enabled: false,
          default_sandbox_workflows_read_enabled: false,
          default_sandbox_workflows_write_enabled: false,
          default_role_ids: [ roles(:acme_infra).oid, roles(:globex_infra).oid ]
        )

        assert_response :ok
        settings = system_settings(:default).reload
        assert_equal "public", settings.default_sandbox_repo_cache
        assert_equal false, settings.default_sandbox_observability_enabled
        assert_equal false, settings.default_sandbox_sessions_read_enabled
        assert_equal false, settings.default_sandbox_workflows_read_enabled
        assert_equal false, settings.default_sandbox_workflows_write_enabled
        assert_predicate roles(:acme_infra).reload, :assign_by_default?
        assert_predicate roles(:globex_infra).reload, :assign_by_default?
        assert_not roles(:default_infra).reload.assign_by_default?
        assert_equal [ roles(:acme_infra).oid, roles(:globex_infra).oid ].sort, json_body.dig("data", "default_role_ids").sort
      end

      test "admin can clear all default roles" do
        roles(:acme_infra).update!(assign_by_default: true)

        patch_settings(default_role_ids: [])

        assert_response :ok
        assert_empty Role.where(assign_by_default: true)
      end

      test "updating sandbox defaults without role params preserves default roles" do
        role = roles(:acme_infra)
        role.update!(assign_by_default: true)

        patch_settings(default_sandbox_repo_cache: "public")

        assert_response :ok
        assert_predicate role.reload, :assign_by_default?
      end

      test "invalid settings are rejected without changing default roles" do
        roles(:acme_infra).update!(assign_by_default: true)

        patch_settings(default_sandbox_repo_cache: "everything", default_role_ids: [])

        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "default_sandbox_repo_cache").present?
        assert_not_equal "everything", system_settings(:default).reload.default_sandbox_repo_cache
        assert_predicate roles(:acme_infra).reload, :assign_by_default?
      end

      test "unknown default role ids are rejected" do
        patch_settings(default_sandbox_repo_cache: "none", default_role_ids: [ "role_missing" ])

        assert_response :unprocessable_entity
        assert_match(/role_missing/, json_body.dig("error", "details", "default_role_ids").first)
        assert_equal "all", system_settings(:default).reload.default_sandbox_repo_cache
      end

      test "admin sees the allowed Slack Connect workspaces" do
        use_api_client(FakeApiClient.new(team_ids: %w[T0PARTNER1 T0PARTNER2]))

        get slack_external_teams_api_v1_system_settings_path

        assert_response :ok
        assert_equal %w[T0PARTNER1 T0PARTNER2], json_body.dig("data", "slack_external_team_ids")
      end

      test "reports when the Slack Connect workspaces cannot be loaded" do
        use_api_client(FakeApiClient.new(error: Errno::ECONNREFUSED.new))

        get slack_external_teams_api_v1_system_settings_path

        assert_response :service_unavailable
        assert_match(/could not be loaded/, json_body.dig("error", "message"))
      end

      test "admin replaces the allowed Slack Connect workspaces" do
        client = FakeApiClient.new
        use_api_client(client)

        put slack_external_teams_api_v1_system_settings_path,
            params: { data: { slack_external_team_ids: "T0PARTNER1, t0partner2\nT0PARTNER3" } },
            headers: csrf_headers, as: :json

        assert_response :ok
        assert_equal %w[T0PARTNER1 t0partner2 T0PARTNER3], client.replaced
        assert_equal %w[T0PARTNER1 t0partner2 T0PARTNER3], json_body.dig("data", "slack_external_team_ids")

        put slack_external_teams_api_v1_system_settings_path,
            params: { data: { slack_external_team_ids: [ " T0A ", "", "T0B" ] } }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal %w[T0A T0B], client.replaced
      end

      test "an empty list clears the allowed workspaces" do
        client = FakeApiClient.new(team_ids: %w[T0PARTNER1])
        use_api_client(client)

        put slack_external_teams_api_v1_system_settings_path,
            params: { data: { slack_external_team_ids: "" } }, headers: csrf_headers, as: :json

        assert_response :ok
        assert_equal [], client.replaced
      end

      test "returns Slack team id validation errors from the API" do
        use_api_client(FakeApiClient.new(error: CentaurApiClient::Error.new("invalid Slack team ids: C0CHANNEL", status: 400)))

        put slack_external_teams_api_v1_system_settings_path,
            params: { data: { slack_external_team_ids: "T0PARTNER1 C0CHANNEL" } }, headers: csrf_headers, as: :json

        assert_response :unprocessable_entity
        assert_equal "invalid Slack team ids: C0CHANNEL", json_body.dig("error", "message")
      end

      test "reports an unavailable Centaur API when saving Slack Connect workspaces" do
        use_api_client(FakeApiClient.new(error: CentaurApiClient::Error.new("boom", status: 500)))

        put slack_external_teams_api_v1_system_settings_path,
            params: { data: { slack_external_team_ids: "T0PARTNER1" } }, headers: csrf_headers, as: :json

        assert_response :service_unavailable
        assert_equal "Slack Connect workspaces could not be saved because the Centaur API is unavailable.",
                     json_body.dig("error", "message")
      end
    end
  end
end
