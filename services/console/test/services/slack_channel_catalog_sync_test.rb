require "test_helper"

class SlackChannelCatalogSyncTest < ActiveJob::TestCase
  TOKEN = "xoxb-test-token"
  API_URL = "https://slack.test/api"
  TEAM_ID = "T0123456789"
  BOT_USER_ID = "U0999999999"
  HEADERS = { "Authorization" => "Bearer #{TOKEN}" }.freeze

  setup do
    SlackBotChannel.delete_all
  end

  test "empty catalogs enqueue one channel sync" do
    cache = ActiveSupport::Cache::MemoryStore.new

    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => TOKEN) do
      Rails.stub(:cache, cache) do
        assert_enqueued_jobs 1, only: SlackChannelCatalogRefreshJob do
          2.times { SlackChannelCatalogSync.enqueue_if_empty }
        end
      end
    end
  end

  test "sync channels imports the complete list and deactivates missing channels" do
    old = create_channel(channel_id: "C0000000001", name: "old")
    sync = build_sync

    sync.stub(:fetch_identity, identity) do
      sync.stub(:fetch_channels, [ remote_channel(id: "C0123456789", name: "general") ]) do
        assert_equal 1, sync.sync_channels
      end
    end

    assert_not old.reload.active
    assert SlackBotChannel.find_by!(channel_id: "C0123456789").active
  end

  test "failed channel sync retains existing rows" do
    channel = create_channel(channel_id: "C0123456789", name: "general")
    sync = build_sync

    error = assert_raises(SlackApi::Error) do
      sync.stub(:fetch_identity, -> { raise SlackApi::Error, "Slack unavailable" }) do
        sync.sync_channels
      end
    end

    assert_equal "Slack unavailable", error.message
    assert channel.reload.active
  end

  test "import channel creates a newly used channel" do
    sync = build_sync

    channel = sync.stub(:fetch_identity, identity) do
      sync.stub(:fetch_channel, remote_channel(id: "C0123456789", name: "general")) do
        sync.import_channel("C0123456789")
      end
    end

    assert_equal "general", channel.name
    assert channel.active
  end

  test "import channel treats inaccessible private channels as not joined" do
    existing = create_channel(channel_id: "G0123456789", name: "private")
    api = Minitest::Mock.new
    api.expect(
      :get,
      response(ok: false, error: "channel_not_found"),
      [ "#{API_URL}/conversations.info" ],
      params: { channel: existing.channel_id },
      headers: HEADERS
    )

    assert_nil build_sync(api: api).import_channel(existing.channel_id)
    assert_not existing.reload.active
    api.verify
  end

  test "channel members replaces only a complete membership array" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    sync = build_sync

    members = sync.stub(:fetch_member_user_ids, [ BOT_USER_ID, "U_NEW" ]) do
      sync.channel_members(channel.channel_id)
    end

    assert_equal [ BOT_USER_ID, "U_NEW" ], members
    assert_equal members, channel.reload.member_user_ids
    assert_nil channel.membership_error
  end

  test "channel members stores complete membership when the bot is not joined" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    sync = build_sync

    members = sync.stub(:fetch_member_user_ids, [ "U_NEW" ]) { sync.channel_members(channel.channel_id) }

    assert_equal [ "U_NEW" ], members
    assert_equal members, channel.reload.member_user_ids
    assert_nil channel.membership_error
  end

  test "rate limits preserve membership for a retry" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    sync = build_sync
    fetch_members = ->(*) { raise SlackApi::RateLimitedError.new("rate limited", retry_after: 12) }

    error = assert_raises(SlackApi::RetryableError) do
      sync.stub(:fetch_member_user_ids, fetch_members) { sync.channel_members(channel.channel_id) }
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
    sync = build_sync
    fetch_members = lambda do |channel_id|
      refreshed << channel_id
      [ BOT_USER_ID ]
    end

    count = sync.stub(:fetch_member_user_ids, fetch_members) { sync.sync_memberships }

    assert_equal SlackChannelCatalogSync::MEMBERSHIP_BATCH_SIZE, count
    assert_equal SlackChannelCatalogSync::MEMBERSHIP_BATCH_SIZE, refreshed.length
  end

  test "Slack requests paginate channels and configure short timeouts" do
    api = Minitest::Mock.new
    api.expect(:get, response(ok: true, team_id: TEAM_ID, user_id: BOT_USER_ID),
               [ "#{API_URL}/auth.test" ], params: {}, headers: HEADERS)
    api.expect(
      :get,
      response(ok: true, channels: [
        { id: "C0123456789", name: "general", is_private: false },
        { id: "D0123456789", user: "U0123456789", is_im: true }
      ]),
      [ "#{API_URL}/conversations.list" ],
      params: {
        types: SlackChannelCatalogSync::CHANNEL_TYPES,
        exclude_archived: "false",
        limit: "200"
      },
      headers: HEADERS
    )
    captured_options = nil

    HttpClient.stub(:new, ->(**options) { captured_options = options; api }) do
      assert_equal 1, build_sync.sync_channels
    end

    assert_equal [ "C0123456789" ], SlackBotChannel.pluck(:channel_id)
    assert_equal SlackChannelCatalogSync::OPEN_TIMEOUT_SECONDS, captured_options.fetch(:open_timeout)
    assert_equal SlackChannelCatalogSync::READ_TIMEOUT_SECONDS, captured_options.fetch(:read_timeout)
    assert_equal SlackChannelCatalogSync::WRITE_TIMEOUT_SECONDS, captured_options.fetch(:write_timeout)
    api.verify
  end

  test "Slack membership requests fully paginate" do
    channel = create_channel(channel_id: "C0123456789", name: "general")
    api = Minitest::Mock.new
    api.expect(:get, response(ok: true, members: [ BOT_USER_ID, "U1111111111" ],
                              response_metadata: { next_cursor: "next" }),
               [ "#{API_URL}/conversations.members" ],
               params: { channel: channel.channel_id, limit: "200" }, headers: HEADERS)
    api.expect(:get, response(ok: true, members: %w[U2222222222 U1111111111],
                              response_metadata: { next_cursor: "" }),
               [ "#{API_URL}/conversations.members" ],
               params: { channel: channel.channel_id, limit: "200", cursor: "next" }, headers: HEADERS)

    members = build_sync(api: api).channel_members(channel.channel_id)

    assert_equal [ BOT_USER_ID, "U1111111111", "U2222222222" ], members
    api.verify
  end

  private

  def build_sync(api: nil)
    SlackChannelCatalogSync.new(token: TOKEN, api_url: API_URL, api: api)
  end

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
    SlackChannelCatalogSync::Identity.new(team_id: TEAM_ID, bot_user_id: BOT_USER_ID)
  end

  def remote_channel(id:, name:, private: false, archived: false)
    SlackChannelCatalogSync::RemoteChannel.new(id: id, name: name, private: private, archived: archived)
  end

  def response(payload)
    HttpClient::Response.new(status: 200, body: payload.to_json)
  end
end
