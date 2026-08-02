require "test_helper"
require Rails.root.join("db/migrate/20260731120000_add_identity_fields_to_principals")

class AddIdentityFieldsToPrincipalsTest < ActiveSupport::TestCase
  test "infers canonical kinds and otherwise preserves known legacy kinds" do
    cases = {
      "warm-pool-bootstrap" => [ {}, "unknown" ],
      "workflow-host" => [ {}, "unknown" ],
      "console-user-ada-123" => [ {}, "console_user" ],
      "workflow-daily-report" => [ {}, "workflow" ],
      "discord-channel-guild-channel" => [ {}, "unknown" ],
      "linear-issue-issue-123" => [ {}, "unknown" ],
      "teams-user-user-123" => [ {}, "unknown" ],
      "teams-conversation-conversation-123" => [ {}, "unknown" ],
      "slack-user-t123-u123" => [ { "kind" => "user" }, "slack_dm" ],
      "slack-user-e123-u123" => [ {}, "slack_dm" ],
      "slack-user-u123" => [ { "kind" => "slack_dm" }, "unknown" ],
      "slack-user-legacy-identity" => [ { "kind" => "user" }, "unknown" ],
      "slack-channel-t123-d123" => [ {}, "slack_dm" ],
      "slack-channel-e123-d123" => [ {}, "slack_dm" ],
      "slack-channel-d123" => [ { "kind" => "slack_dm" }, "unknown" ],
      "slack-channel-c123" => [ { "kind" => "user" }, "slack_channel" ],
      "slack-channel-t123-c123" => [ {}, "slack_channel" ],
      "slack-channel-g123" => [ {}, "slack_channel" ],
      "U987654321" => [ { "kind" => "user" }, "user" ],
      "U-alice" => [ { "kind" => "user" }, "user" ],
      "thread-slack-c123-ts" => [ {}, "unknown" ],
      "manually-created" => [ { "kind" => "service" }, "unknown" ],
      "invalid-kind" => [ { "kind" => "future_platform" }, "unknown" ]
    }

    values = cases.map do |foreign_id, (labels, _kind)|
      "(#{connection.quote(foreign_id)}, #{connection.quote(labels.to_json)}::jsonb)"
    end.join(", ")
    rows = connection.select_rows(<<~SQL.squish)
      SELECT foreign_id, #{AddIdentityFieldsToPrincipals::KIND_FROM_FOREIGN_ID_SQL} AS kind
      FROM (VALUES #{values}) AS principals(foreign_id, labels)
    SQL

    assert_equal cases.transform_values(&:last), rows.to_h
  end

  test "preserves Slack identity values exactly regardless of format" do
    labels = {
      "slack_user_id" => " U12345 ",
      "slack_channel_id" => " D123 ",
      "slack_team_id" => " TACME ",
      "slack_email" => " pending "
    }
    row = connection.select_one(<<~SQL.squish)
      SELECT
        #{AddIdentityFieldsToPrincipals::SLACK_USER_ID_SQL} AS slack_user_id,
        #{AddIdentityFieldsToPrincipals::SLACK_CHANNEL_ID_SQL} AS slack_channel_id,
        #{AddIdentityFieldsToPrincipals::SLACK_TEAM_ID_SQL} AS slack_team_id,
        #{AddIdentityFieldsToPrincipals::SLACK_EMAIL_SQL} AS slack_email
      FROM (VALUES (#{connection.quote(labels.to_json)}::jsonb)) AS principals(labels)
    SQL

    assert_equal(
      {
        "slack_user_id" => " U12345 ",
        "slack_channel_id" => " D123 ",
        "slack_team_id" => " TACME ",
        "slack_email" => " pending "
      },
      row
    )
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

  test "restores identity columns to labels for rollback" do
    labels = { "team" => "platform", "nullable" => nil }
    restored = connection.select_value(<<~SQL.squish)
      SELECT #{AddIdentityFieldsToPrincipals::RESTORED_LABELS_SQL}
      FROM (VALUES (
        #{connection.quote(labels.to_json)}::jsonb,
        'user', 'U12345', NULL, 'TACME', 'pending'
      )) AS principals(labels, kind, slack_user_id, slack_channel_id, slack_team_id, slack_email)
    SQL

    assert_equal(
      labels.merge(
        "kind" => "user",
        "slack_user_id" => "U12345",
        "slack_team_id" => "TACME",
        "slack_email" => "pending"
      ),
      JSON.parse(restored)
    )
  end

  private

  def connection
    ActiveRecord::Base.connection
  end
end
