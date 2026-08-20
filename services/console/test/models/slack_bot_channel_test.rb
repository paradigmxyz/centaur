require "test_helper"

class SlackBotChannelTest < ActiveSupport::TestCase
  test "catalog scopes filter and order channels" do
    channel = slack_bot_channels(:general)
    SlackBotChannel.create!(
      team_id: channel.team_id,
      bot_user_id: channel.bot_user_id,
      channel_id: "C1111111111",
      name: "general-without-bot",
      member_user_ids: [ "U0123456789" ]
    )

    assert_equal 2, SlackBotChannel.active.matching("GEN").count
    assert_empty SlackBotChannel.active.excluding_channel_ids([ channel.channel_id ]).matching(channel.channel_id)
    assert_equal [ channel ], SlackBotChannel.active.with_members([ "U0123456789" ]).matching("GEN").to_a
  end

  test "channel identity is unique within a team" do
    duplicate = slack_bot_channels(:general).dup

    refute duplicate.valid?
    assert_includes duplicate.errors[:channel_id], "has already been taken"
  end
end
