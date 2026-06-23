require "test_helper"

class PrincipalRoleTest < ActiveSupport::TestCase
  test "is valid when principal and role share a namespace" do
    pr = PrincipalRole.new(principal: principals(:acme_user_bob), role: roles(:acme_admin_role))
    assert pr.valid?
  end

  test "rejects a role from a different namespace" do
    pr = PrincipalRole.new(principal: principals(:globex_user), role: roles(:acme_infra))
    assert_not pr.valid?
    assert_includes pr.errors[:role], "must be in the same namespace as the principal"
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
