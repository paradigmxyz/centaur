require "test_helper"

class SystemSettingTest < ActiveSupport::TestCase
  test "current returns the singleton settings row" do
    assert_equal system_settings(:default), SystemSetting.current
  end

  test "defaults enable all sandbox capabilities" do
    SystemSetting.delete_all

    settings = SystemSetting.current

    assert_equal "all", settings.default_sandbox_repo_cache
    assert_equal true, settings.default_sandbox_observability_enabled
    assert_equal true, settings.default_sandbox_api_server_enabled
  end

  test "repo-cache setting is normalized and validated" do
    settings = system_settings(:default)

    settings.update!(default_sandbox_repo_cache: "pub")
    assert_equal "public", settings.default_sandbox_repo_cache

    settings.default_sandbox_repo_cache = "invalid"
    assert_predicate settings, :valid?
    assert_equal "all", settings.default_sandbox_repo_cache
  end

  test "only one settings row can exist" do
    duplicate = SystemSetting.new

    assert_not duplicate.valid?
    assert_includes duplicate.errors[:singleton], "has already been taken"
  end
end
