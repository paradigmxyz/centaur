require "test_helper"

class RoleTest < ActiveSupport::TestCase
  def valid_attrs(overrides = {})
    {
      namespace: "acme",
      foreign_id: "observability",
      name: "Observability",
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  test "is valid with namespace and created_by" do
    assert Role.new(valid_attrs).valid?
  end

  test "requires a creator for operator-managed roles" do
    role = Role.new(valid_attrs(created_by: nil))

    assert_not role.valid?
    assert_includes role.errors[:created_by], "must exist"
  end

  test "the system-managed infra role is the initial default" do
    assert_predicate roles(:default_infra), :assign_by_default?
  end

  test "ensure_default_infra creates the role when missing" do
    roles(:default_infra).destroy!
    owner = users(:acme_admin)

    role = Role.ensure_default_infra!(created_by: owner)

    assert_equal "default", role.namespace
    assert_equal "infra", role.foreign_id
    assert_equal "Infra", role.name
    assert_equal({ "managed-by" => "centaur" }, role.labels)
    assert_predicate role, :assign_by_default?
    assert_equal owner, role.created_by
  end

  test "ensure_default_infra preserves an existing role and marks it default" do
    role = roles(:default_infra)
    role.update!(name: "Custom Infra", assign_by_default: false)

    result = Role.ensure_default_infra!(created_by: users(:globex_admin))

    assert_equal role, result
    assert_equal "Custom Infra", result.name
    assert_equal users(:acme_admin), result.created_by
    assert_predicate result, :assign_by_default?
  end

  test "replace_default_assignments updates the selected roles" do
    selected = roles(:acme_infra)

    Role.replace_default_assignments!([ selected.id ])

    assert_predicate selected.reload, :assign_by_default?
    assert_not roles(:default_infra).reload.assign_by_default?
  end

  test "requires namespace" do
    role = Role.new(valid_attrs(namespace: nil))
    assert_not role.valid?
    assert_includes role.errors[:namespace], "can't be blank"
  end

  test "rejects non-url-safe namespace" do
    role = Role.new(valid_attrs(namespace: "bad space"))
    assert_not role.valid?
    assert_includes role.errors[:namespace], Role::URL_SAFE_MESSAGE
  end

  test "foreign_id is unique per namespace" do
    dup = Role.new(valid_attrs(foreign_id: "infra"))
    assert_not dup.valid?
    assert_includes dup.errors[:foreign_id], "has already been taken"
  end

  test "allows the same foreign_id in different namespaces" do
    assert Role.new(valid_attrs(namespace: "globex", foreign_id: "admin")).valid?
  end

  test "allows a nil foreign_id" do
    assert Role.new(valid_attrs(foreign_id: nil)).valid?
  end

  test "rejects a foreign_id that starts with the opaque id prefix" do
    role = Role.new(valid_attrs(foreign_id: "role_abc123"))
    assert_not role.valid?
    assert_includes role.errors[:foreign_id], "must not start with \"role_\", which is reserved for opaque ids"
  end

  test "namespace and foreign_id are immutable after creation" do
    role = roles(:acme_infra)
    assert_raises(ActiveRecord::ReadonlyAttributeError) { role.update!(namespace: "globex") }
    assert_raises(ActiveRecord::ReadonlyAttributeError) { role.update!(foreign_id: "other") }
  end

  test "destroys its grants when destroyed" do
    role = roles(:acme_infra)
    grant_ids = role.grants.pluck(:id)
    assert_not_empty grant_ids
    role.destroy!
    assert_equal 0, Grant.where(id: grant_ids).count
  end

  test "destroys its role assignments when destroyed" do
    role = roles(:acme_infra)
    assert_not_empty role.principal_roles.pluck(:id)
    role.destroy!
    assert_equal 0, PrincipalRole.where(role_id: role.id).count
  end

  test "declares role as its oid prefix" do
    assert_equal "role", Role.oid_prefix
  end

  test "find_by_oid round-trips" do
    role = roles(:acme_infra)
    assert_equal role, Role.find_by_oid(role.oid)
  end
end
