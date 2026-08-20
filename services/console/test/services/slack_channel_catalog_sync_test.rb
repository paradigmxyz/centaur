require "test_helper"

class SlackChannelCatalogSyncTest < ActiveSupport::TestCase
  TEAM_ID = "T0123456789"
  BOT_USER_ID = "U0999999999"

  setup do
    SlackBotChannel.delete_all
  end

  test "sync channels imports the complete list and deactivates missing channels" do
    old = create_channel(channel_id: "C0000000001", name: "old")
    client = mock_client
    client.expect(:fetch_identity, identity)
    client.expect(:fetch_channels, [ remote_channel(id: "C0123456789", name: "general") ])

    assert_equal 1, SlackChannelCatalogSync.new(client: client).sync_channels

    assert_not old.reload.active
    assert SlackBotChannel.find_by!(channel_id: "C0123456789").active
    client.verify
  end

  test "failed channel sync retains existing rows" do
    channel = create_channel(channel_id: "C0123456789", name: "general")
    client = Object.new
    client.define_singleton_method(:fetch_identity) { raise SlackApi::Error, "Slack unavailable" }

    error = assert_raises(SlackApi::Error) do
      SlackChannelCatalogSync.new(client: client).sync_channels
    end

    assert_equal "Slack unavailable", error.message
    assert channel.reload.active
  end

  test "import channel creates a newly used channel" do
    client = mock_client
    client.expect(:fetch_identity, identity)
    client.expect(:fetch_channel, remote_channel(id: "C0123456789", name: "general"), [ "C0123456789" ])

    channel = SlackChannelCatalogSync.new(client: client).import_channel("C0123456789")

    assert_equal "general", channel.name
    assert channel.active
    client.verify
  end

  test "channel members replaces only a complete membership array" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    client = mock_client
    client.expect(:fetch_member_user_ids, [ BOT_USER_ID, "U_NEW" ], [ channel.channel_id ])

    members = SlackChannelCatalogSync.new(client: client).channel_members(channel.channel_id)

    assert_equal [ BOT_USER_ID, "U_NEW" ], members
    assert_equal members, channel.reload.member_user_ids
    assert_nil channel.membership_error
    client.verify
  end

  test "incomplete membership keeps the previous array and records an error" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    client = mock_client
    client.expect(:fetch_member_user_ids, [ "U_NEW" ], [ channel.channel_id ])

    assert_raises(SlackApi::Error) do
      SlackChannelCatalogSync.new(client: client).channel_members(channel.channel_id)
    end

    assert_equal [ BOT_USER_ID, "U_OLD" ], channel.reload.member_user_ids
    assert_match(/omitted the bot user/, channel.membership_error)
    client.verify
  end

  test "rate limits preserve membership for a retry" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    client = Object.new
    client.define_singleton_method(:fetch_member_user_ids) do |_channel_id|
      raise SlackApi::RateLimitedError.new("rate limited", retry_after: 12)
    end

    error = assert_raises(SlackApi::RetryableError) do
      SlackChannelCatalogSync.new(client: client).channel_members(channel.channel_id)
    end

    assert_equal 12, error.retry_after
    assert_equal [ BOT_USER_ID, "U_OLD" ], channel.reload.member_user_ids
    assert_nil channel.membership_last_attempted_at
  end

  test "membership sync processes at most one batch" do
    (SlackChannelCatalogSync::MEMBERSHIP_BATCH_SIZE + 1).times do |index|
      create_channel(
        channel_id: format("C%010d", index),
        name: "channel-#{index}",
        membership_refreshed_at: nil
      )
    end
    refreshed = []
    client = Object.new
    client.define_singleton_method(:fetch_member_user_ids) do |channel_id|
      refreshed << channel_id
      [ BOT_USER_ID ]
    end

    count = SlackChannelCatalogSync.new(client: client).sync_memberships

    assert_equal SlackChannelCatalogSync::MEMBERSHIP_BATCH_SIZE, count
    assert_equal SlackChannelCatalogSync::MEMBERSHIP_BATCH_SIZE, refreshed.length
  end

  private

  def create_channel(channel_id:, name:, member_user_ids: [ BOT_USER_ID ],
                     membership_refreshed_at: Time.current)
    SlackBotChannel.create!(
      team_id: TEAM_ID,
      bot_user_id: BOT_USER_ID,
      channel_id: channel_id,
      name: name,
      private: false,
      archived: false,
      active: true,
      member_user_ids: member_user_ids,
      membership_refreshed_at: membership_refreshed_at,
      last_seen_at: Time.current
    )
  end

  def identity
    SlackChannelCatalog::Identity.new(team_id: TEAM_ID, bot_user_id: BOT_USER_ID)
  end

  def remote_channel(id:, name:, private: false, archived: false)
    SlackChannelCatalog::RemoteChannel.new(id: id, name: name, private: private, archived: archived)
  end

  def mock_client
    Minitest::Mock.new
  end
end
