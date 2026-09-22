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
      assert_select "input[name='system_setting[default_sandbox_sessions_read_enabled]']"
      assert_select "input[name='system_setting[default_sandbox_workflows_read_enabled]']"
      assert_select "input[name='system_setting[default_sandbox_workflows_write_enabled]']"
      assert_select "input[name='system_setting[default_role_ids][]'][value=?]", roles(:acme_infra).id.to_s
      assert_select "textarea[name='system_setting[organization_instructions_draft]']"
      assert_select "button[name='publish_organization_instructions'][value='1']", text: "Publish to agents"
    end

    test "admin saves organization instructions as a draft without publishing" do
      sign_in users(:acme_admin)

      assert_no_difference -> { OrganizationInstructionVersion.count } do
        patch console_system_settings_url,
              params: {
                system_setting: { organization_instructions_draft: "Search the canonical workspace first." }
              }
      end

      assert_redirected_to edit_console_system_settings_path
      assert_equal "Draft saved.", flash[:notice]
      settings = system_settings(:default).reload
      assert_equal "Search the canonical workspace first.", settings.organization_instructions_draft
      assert_nil settings.published_organization_instruction_version
    end

    test "admin publishes a versioned organization instruction" do
      sign_in users(:acme_admin)

      assert_difference -> { OrganizationInstructionVersion.count }, 1 do
        patch console_system_settings_url,
              params: {
                publish_organization_instructions: "1",
                system_setting: { organization_instructions_draft: "Search Notion for current workstreams." }
              }
      end

      assert_redirected_to edit_console_system_settings_path
      assert_equal "Organization instructions published.", flash[:notice]
      settings = system_settings(:default).reload
      version = settings.published_organization_instruction_version
      assert_equal "Search Notion for current workstreams.", version.content
      assert_equal users(:acme_admin), version.published_by
    end

    test "admin restores a historical version by publishing a new version" do
      sign_in users(:acme_admin)
      old_version = OrganizationInstructionVersion.create!(
        content: "Use the canonical source.",
        published_by: users(:acme_admin)
      )
      settings = system_settings(:default)
      settings.update!(organization_instructions_draft: "Newer draft")

      assert_difference -> { OrganizationInstructionVersion.count }, 1 do
        post console_restore_organization_instruction_version_url(old_version)
      end

      assert_redirected_to edit_console_system_settings_path
      assert_equal "Organization instructions restored and published.", flash[:notice]
      settings.reload
      refute_equal old_version, settings.published_organization_instruction_version
      assert_equal old_version.content, settings.published_organization_instruction_version.content
      assert_equal old_version.content, settings.organization_instructions_draft
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
  end
end
