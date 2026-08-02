require "test_helper"
require Rails.root.join("db/migrate/20260802170149_add_console_user_identity_fields_to_principals")

class AddConsoleUserIdentityFieldsToPrincipalsTest < ActiveSupport::TestCase
  test "preserves console user identity values exactly regardless of format" do
    labels = {
      "console-user-id" => " stale-user-id ",
      "email" => " pending ",
      "managed-by" => "centaur"
    }
    row = connection.select_one(<<~SQL.squish)
      SELECT
        #{AddConsoleUserIdentityFieldsToPrincipals::CONSOLE_USER_ID_SQL} AS console_user_id,
        #{AddConsoleUserIdentityFieldsToPrincipals::CONSOLE_USER_EMAIL_SQL} AS console_user_email,
        #{AddConsoleUserIdentityFieldsToPrincipals::ORDINARY_LABELS_SQL} AS labels
      FROM (VALUES (#{connection.quote(labels.to_json)}::jsonb)) AS principals(labels)
    SQL

    assert_equal " stale-user-id ", row.fetch("console_user_id")
    assert_equal " pending ", row.fetch("console_user_email")
    assert_equal({ "managed-by" => "centaur" }, JSON.parse(row.fetch("labels")))
  end

  test "restores console user identity columns to labels for rollback" do
    labels = { "managed-by" => "centaur", "nullable" => nil }
    restored = connection.select_value(<<~SQL.squish)
      SELECT #{AddConsoleUserIdentityFieldsToPrincipals::RESTORED_LABELS_SQL}
      FROM (VALUES (
        #{connection.quote(labels.to_json)}::jsonb,
        'usr_12345678', 'ada@example.com'
      )) AS principals(labels, console_user_id, console_user_email)
    SQL

    assert_equal(
      labels.merge("console-user-id" => "usr_12345678", "email" => "ada@example.com"),
      JSON.parse(restored)
    )
  end

  private

  def connection
    ActiveRecord::Base.connection
  end
end
