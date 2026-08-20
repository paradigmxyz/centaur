require "test_helper"

class SlackBotChannelTest < ActiveSupport::TestCase
  test "catalog scopes filter and order channels" do
    channel = slack_bot_channels(:general)

    assert_equal [ channel ], SlackBotChannel.active.matching("GEN").ordered.to_a
    assert_empty SlackBotChannel.active.excluding_channel_ids([ channel.channel_id ]).matching("GEN")
    assert_equal [ channel ], SlackBotChannel.active.with_members([ "U0123456789" ]).matching("GEN").to_a
  end

  test "channel identity is unique within a team" do
    duplicate = slack_bot_channels(:general).dup

    refute duplicate.valid?
    assert_includes duplicate.errors[:channel_id], "has already been taken"
  end
end
