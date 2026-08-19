require "test_helper"
require Rails.root.join("db/migrate/20260802170149_add_console_user_identity_fields_to_principals")

class AddConsoleUserIdentityFieldsToPrincipalsTest < ActiveSupport::TestCase
  test "decodes existing console user oids and maps stale references to nil" do
    user = users(:acme_admin)
    stale_oid = User.new(id: User.maximum(:id) + 1_000).oid
    migration = AddConsoleUserIdentityFieldsToPrincipals.new

    assert_equal user.id, migration.send(:console_user_id_from_oid, user.oid)
    assert_nil migration.send(:console_user_id_from_oid, stale_oid)
    assert_nil migration.send(:console_user_id_from_oid, "not-an-oid")
  end

  test "removes migrated identity values from labels" do
    labels = {
      "console-user-id" => "usr_12345678",
      "email" => "ada@example.com",
      "managed-by" => "centaur"
    }
    migration = AddConsoleUserIdentityFieldsToPrincipals.new

    assert_equal({ "managed-by" => "centaur" }, migration.send(:ordinary_labels, labels))
  end

  test "encodes console user ids back to oids for rollback" do
    user = users(:acme_admin)
    migration = AddConsoleUserIdentityFieldsToPrincipals.new

    assert_equal user.oid, migration.send(:console_user_oid_from_id, user.id)
    assert_nil migration.send(:console_user_oid_from_id, User.maximum(:id) + 1_000)
  end
end
