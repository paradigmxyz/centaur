require "test_helper"

class PrincipalRoleTest < ActiveSupport::TestCase
  test "is valid with a principal and role" do
    pr = PrincipalRole.new(principal: principals(:acme_user_bob), role: roles(:acme_admin_role))
    assert pr.valid?
  end

  test "allows any role to be assigned" do
    pr = PrincipalRole.new(principal: principals(:globex_user), role: roles(:acme_infra))
    assert pr.valid?
  end

  test "rejects assigning the same role twice" do
    dup = PrincipalRole.new(principal: principals(:acme_channel), role: roles(:acme_infra))
    assert_not dup.valid?
    assert_includes dup.errors[:role_id], "is already assigned to this principal"
  end

  test "requires a principal and a role" do
    pr = PrincipalRole.new
    assert_not pr.valid?
    assert_includes pr.errors[:principal], "must exist"
    assert_includes pr.errors[:role], "must exist"
  end
end
