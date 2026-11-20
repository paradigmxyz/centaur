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

  test "a caller that loses the creation race reuses the winner's principal" do
    user = users(:member_user)
    winner = ConsoleUserPrincipalProvisioner.call(user)
    find_or_initialize_by = Principal.method(:find_or_initialize_by)
    calls = 0
    # The first lookup misses, as if it ran before the concurrent winner
    # committed; the save then fails the uniqueness validation.
    stale_lookup = lambda do |attributes|
      calls += 1
      calls == 1 ? Principal.new(attributes) : find_or_initialize_by.call(attributes)
    end

    Principal.stub(:find_or_initialize_by, stale_lookup) do
      assert_no_difference -> { Principal.count } do
        assert_equal winner, ConsoleUserPrincipalProvisioner.call(user)
      end
    end
    assert_equal 2, calls
  end

  test "repeated calls leave an unchanged principal untouched" do
    user = users(:member_user)
    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_no_changes -> { principal.reload.updated_at } do
      ConsoleUserPrincipalProvisioner.call(user)
    end
  end
end
