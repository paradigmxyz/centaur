require "test_helper"

class SlackBotChannelTest < ActiveSupport::TestCase
  test "catalog channel preserves the provider contract" do
    channel = slack_bot_channels(:general).catalog_channel

    assert_equal "C0123456789", channel.id
    assert_equal "general", channel.name
    refute channel.private
  end

  test "channel identity is unique within a team" do
    duplicate = slack_bot_channels(:general).dup

    refute duplicate.valid?
    assert_includes duplicate.errors[:channel_id], "has already been taken"
  end
end
