require "test_helper"
require Rails.root.join("db/migrate/20260731120000_add_identity_fields_to_principals")

class AddIdentityFieldsToPrincipalsTest < ActiveSupport::TestCase
  test "infers kind only from canonical foreign ID shapes" do
    expected = {
      "warm-pool-bootstrap" => "service",
      "workflow-host" => "service",
      "console-user-ada-123" => "console_user",
      "workflow-daily-report" => "workflow",
      "discord-channel-guild-channel" => "discord_channel",
      "linear-issue-issue-123" => "linear_issue",
      "teams-user-user-123" => "teams_user",
      "teams-conversation-conversation-123" => "teams_conversation",
      "slack-user-t123-u123" => "slack_dm",
      "slack-user-u123" => "unknown",
      "slack-user-legacy-identity" => "unknown",
      "slack-channel-t123-d123" => "slack_dm",
      "slack-channel-d123" => "unknown",
      "slack-channel-c123" => "slack_channel",
      "slack-channel-t123-c123" => "slack_channel",
      "slack-channel-g123" => "slack_channel",
      "thread-slack-c123-ts" => "unknown",
      "manually-created" => "unknown"
    }

    values = expected.keys.map { |foreign_id| "(#{connection.quote(foreign_id)})" }.join(", ")
    rows = connection.select_rows(<<~SQL.squish)
      SELECT foreign_id, #{AddIdentityFieldsToPrincipals::KIND_FROM_FOREIGN_ID_SQL} AS kind
      FROM (VALUES #{values}) AS principals(foreign_id)
    SQL

    assert_equal expected, rows.to_h
  end

  test "removes identity aliases from persisted labels" do
    labels = {
      "kind" => "slack_dm",
      "slack_user_id" => "U123",
      "slack_channel_id" => "D123",
      "slack_team_id" => "T123",
      "slack_email" => "ada@example.com",
      "team" => "platform"
    }
    ordinary_labels = connection.select_value(<<~SQL.squish)
      SELECT #{AddIdentityFieldsToPrincipals::ORDINARY_LABELS_SQL}
      FROM (VALUES (#{connection.quote(labels.to_json)}::jsonb)) AS principals(labels)
    SQL

    assert_equal({ "team" => "platform" }, JSON.parse(ordinary_labels))
  end

  private

  def connection
    ActiveRecord::Base.connection
  end
end
