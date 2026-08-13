require "test_helper"
require Rails.root.join("db/migrate/20260813181200_backfill_slack_dm_principal_console_users")

class BackfillSlackDmPrincipalConsoleUsersTest < ActiveSupport::TestCase
  test "links only unlinked Slack DM principals with matching Console user emails" do
    member = users(:member_user)
    admin = users(:acme_admin)
    matching = create_principal(
      foreign_id: "backfill-matching-slack-dm",
      kind: "slack_dm",
      slack_email: member.email.upcase
    )
    matching.update_columns(slack_email: " #{member.email.upcase} ")
    matching.reload
    unmatched = create_principal(
      foreign_id: "backfill-unmatched-slack-dm",
      kind: "slack_dm",
      slack_email: "unknown@example.com"
    )
    channel = create_principal(
      foreign_id: "backfill-slack-channel",
      kind: "slack_channel",
      slack_email: member.email
    )
    already_linked = create_principal(
      foreign_id: "backfill-already-linked-slack-dm",
      kind: "slack_dm",
      slack_email: member.email,
      console_user: admin
    )
    previous_cache_version = matching.sync_config_cache_version

    BackfillSlackDmPrincipalConsoleUsers.new.up

    assert_equal member, matching.reload.console_user
    assert_equal previous_cache_version + 1, matching.sync_config_cache_version
    assert_nil unmatched.reload.console_user
    assert_nil channel.reload.console_user
    assert_equal admin, already_linked.reload.console_user
  end

  private

  def create_principal(attributes)
    Principal.create!({ created_by: users(:acme_admin) }.merge(attributes))
  end
end
