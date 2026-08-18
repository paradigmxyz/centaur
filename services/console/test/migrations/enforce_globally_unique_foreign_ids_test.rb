require "test_helper"
require Rails.root.join("db/migrate/20260807091441_enforce_globally_unique_foreign_ids")

class EnforceGloballyUniqueForeignIdsTest < ActiveSupport::TestCase
  test "reports every colliding foreign id before adding indexes" do
    migration = EnforceGloballyUniqueForeignIds.new
    results = {
      "roles" => [
        { "foreign_id" => "infra", "record_count" => 3, "namespaces" => "acme, default, globex" }
      ],
      "principals" => [
        { "foreign_id" => "shared-user", "record_count" => 2, "namespaces" => "acme, globex" }
      ]
    }

    migration.stub(:select_all, ->(sql) { results.fetch(sql[/FROM (\w+)/, 1], []) }) do
      error = assert_raises(ActiveRecord::MigrationError) do
        migration.send(:validate_no_foreign_id_collisions!)
      end

      assert_includes error.message,
                      '- principals.foreign_id "shared-user" is used 2 times across namespaces: acme, globex'
      assert_includes error.message,
                      '- roles.foreign_id "infra" is used 3 times across namespaces: acme, default, globex'
      assert_includes error.message, "Rename or remove the colliding foreign IDs, then retry this migration."
    end
  end

  test "passes when foreign ids do not collide" do
    migration = EnforceGloballyUniqueForeignIds.new

    migration.stub(:select_all, []) do
      assert_nil migration.send(:validate_no_foreign_id_collisions!)
    end
  end
end
