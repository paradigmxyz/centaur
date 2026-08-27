require "test_helper"

class ConsoleUserPrincipalProvisionerTest < ActiveSupport::TestCase
  test "creates and then reuses a scoped principal for the Console user" do
    user = users(:member_user)

    assert_difference -> { Principal.count }, 1 do
      @principal = ConsoleUserPrincipalProvisioner.call(user)
    end
    same_principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_equal @principal, same_principal
    assert_equal "console_user", @principal.kind
    assert_equal user, @principal.console_user
    assert_equal "centaur", @principal.labels["managed-by"]
    assert_includes @principal.roles, Role.find_by!(foreign_id: "user-mcp")
  end

  test "repeated calls leave an unchanged principal untouched" do
    user = users(:member_user)
    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_no_changes -> { principal.reload.updated_at } do
      ConsoleUserPrincipalProvisioner.call(user)
    end
  end
end
