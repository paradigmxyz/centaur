require "test_helper"

class SystemSettingTest < ActiveSupport::TestCase
  test "current returns the singleton settings row" do
    assert_equal system_settings(:default), SystemSetting.current
  end

  test "defaults API sandbox capabilities to disabled" do
    SystemSetting.destroy_all

    settings = SystemSetting.current

    assert_equal "all", settings.default_sandbox_repo_cache
    assert_equal true, settings.default_sandbox_observability_enabled
    assert_equal false, settings.default_sandbox_sessions_read_enabled
    assert_equal false, settings.default_sandbox_workflows_read_enabled
    assert_equal false, settings.default_sandbox_workflows_write_enabled
  end

  test "repo-cache setting is validated" do
    settings = system_settings(:default)

    settings.default_sandbox_repo_cache = "invalid"
    assert_not settings.valid?
    assert_includes settings.errors[:default_sandbox_repo_cache], "is not included in the list"
  end

  test "only one settings row can exist" do
    duplicate = SystemSetting.new

    assert_not duplicate.valid?
    assert_includes duplicate.errors[:singleton], "has already been taken"
  end
end
