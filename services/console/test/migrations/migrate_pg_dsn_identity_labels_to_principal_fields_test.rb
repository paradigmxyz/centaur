require "test_helper"
require Rails.root.join("db/migrate/20260810120000_migrate_pg_dsn_identity_labels_to_principal_fields")

class MigratePgDsnIdentityLabelsToPrincipalFieldsTest < ActiveSupport::TestCase
  test "rewrites persisted identity label selectors as fields" do
    secret = pg_dsn_secrets(:acme_analytics_pg)
    secret.update_columns(settings: [
      { "name" => "app.kind", "value_from" => { "principal_label" => "kind" } },
      { "name" => "app.user_id", "value_from" => { "principal_label" => "slack_user_id" } },
      { "name" => "app.channel_id", "value_from" => { "principal_label" => "slack_channel_id" } },
      { "name" => "app.team_id", "value_from" => { "principal_label" => "slack_team_id" } },
      { "name" => "app.email", "value_from" => { "principal_label" => "slack_email" } },
      { "name" => "app.console_user_id", "value_from" => { "principal_label" => "console-user-id" } },
      { "name" => "app.console_email", "value_from" => { "principal_label" => "email" } },
      { "name" => "app.tenant", "value_from" => { "principal_label" => "tenant" } },
      { "name" => "app.literal", "value" => "preserved" }
    ])

    MigratePgDsnIdentityLabelsToPrincipalFields.new.up

    assert_equal [
      { "name" => "app.kind", "value_from" => { "principal_field" => "kind" } },
      { "name" => "app.user_id", "value_from" => { "principal_field" => "slack_user_id" } },
      { "name" => "app.channel_id", "value_from" => { "principal_field" => "slack_channel_id" } },
      { "name" => "app.team_id", "value_from" => { "principal_field" => "slack_team_id" } },
      { "name" => "app.email", "value_from" => { "principal_field" => "slack_email" } },
      { "name" => "app.console_user_id", "value_from" => { "principal_field" => "console_user_id" } },
      { "name" => "app.console_email", "value_from" => { "principal_label" => "email" } },
      { "name" => "app.tenant", "value_from" => { "principal_label" => "tenant" } },
      { "name" => "app.literal", "value" => "preserved" }
    ], secret.reload.settings
  end

  test "down restores identity field selectors as labels" do
    secret = pg_dsn_secrets(:acme_analytics_pg)
    secret.update_columns(settings: [
      { "name" => "app.channel_id", "value_from" => { "principal_field" => "slack_channel_id" } },
      { "name" => "app.console_user_id", "value_from" => { "principal_field" => "console_user_id" } },
      { "name" => "app.principal", "value_from" => { "principal_field" => "foreign_id" } }
    ])

    MigratePgDsnIdentityLabelsToPrincipalFields.new.down

    assert_equal [
      { "name" => "app.channel_id", "value_from" => { "principal_label" => "slack_channel_id" } },
      { "name" => "app.console_user_id", "value_from" => { "principal_label" => "console-user-id" } },
      { "name" => "app.principal", "value_from" => { "principal_field" => "foreign_id" } }
    ], secret.reload.settings
  end
end
