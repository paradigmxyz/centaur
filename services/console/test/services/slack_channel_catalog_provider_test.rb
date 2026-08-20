require "test_helper"

class SlackChannelCatalogProviderTest < ActiveJob::TestCase
  TOKEN = "xoxb-test-token"
  API_URL = "https://slack.test/api"
  TEAM_ID = "T0123456789"
  BOT_USER_ID = "U0999999999"

  setup do
    SlackBotChannel.delete_all
    @cache = ActiveSupport::Cache::MemoryStore.new
  end

  test "cold reads enqueue one refresh without constructing a Slack client" do
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { flunk("request path must not construct the Slack client") }) do
        assert_enqueued_jobs 1, only: SlackChannelCatalogRefreshJob do
          first = SlackChannelCatalogProvider.fetch
          second = SlackChannelCatalogProvider.fetch

          assert_empty first.channels
          assert_match(/loading/, first.error)
          assert_equal first, second
        end
        assert_enqueued_with(job: SlackChannelCatalogRefreshJob, args: [])
      end
    end

    refute_includes key, TOKEN
  end

  test "discovery persists joined channels and deactivates missing rows only after success" do
    old = create_channel(channel_id: "C0000000001", name: "old")
    client = mock_client
    client.expect(:fetch_identity, identity)
    client.expect(:fetch_channels, [ remote_channel(id: "C0123456789", name: "general") ])

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        result = SlackChannelCatalogProvider.refresh

        assert_equal [ "C0123456789" ], result.channels.map(&:id)
      end
    end

    assert_not old.reload.active
    assert SlackBotChannel.find_by!(channel_id: "C0123456789").active
    client.verify
  end

  test "failed discovery retains the last successful rows" do
    channel = create_channel(channel_id: "C0123456789", name: "general")
    client = Object.new
    client.define_singleton_method(:fetch_identity) do
      raise SlackApi::Error, "Slack unavailable"
    end

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        result = SlackChannelCatalogProvider.refresh
        assert_equal "Slack unavailable", result.error
      end

      cached = SlackChannelCatalogProvider.fetch
      assert_equal [ "C0123456789" ], cached.channels.map(&:id)
      assert_nil cached.error
    end

    assert channel.reload.active
  end

  test "search filters by query exclusions and all requested members including the bot" do
    create_channel(
      channel_id: "C0000000001",
      name: "engineering",
      member_user_ids: [ BOT_USER_ID, "U1111111111", "U2222222222" ]
    )
    create_channel(
      channel_id: "C0000000002",
      name: "engineering-private",
      private: true,
      member_user_ids: [ BOT_USER_ID, "U1111111111" ]
    )
    create_channel(
      channel_id: "C0000000003",
      name: "engineering-without-bot",
      member_user_ids: %w[U1111111111 U2222222222]
    )

    with_catalog do
      result = SlackChannelCatalogProvider.search(
        query: "ENGINEER",
        limit: 20,
        exclude_ids: [ "C0000000002" ],
        team_id: TEAM_ID,
        member_user_ids: %w[U1111111111 U2222222222]
      )

      assert_equal [ "C0000000001" ], result.channels.map(&:id)
      assert_nil result.error
    end
  end

  test "membership refresh replaces only complete membership arrays" do
    channel = create_channel(
      channel_id: "C0123456789",
      name: "general",
      member_user_ids: [ BOT_USER_ID, "U_OLD" ],
      membership_refreshed_at: 2.days.ago
    )
    client = mock_client
    client.expect(:fetch_member_user_ids, [ BOT_USER_ID, "U_NEW" ], [ channel.channel_id ])

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        assert_equal 1, SlackChannelCatalogProvider.refresh_memberships
      end
    end

    assert_equal [ BOT_USER_ID, "U_NEW" ], channel.reload.member_user_ids
    assert_nil channel.membership_error
    assert_predicate channel.membership_refreshed_at, :present?
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

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        SlackChannelCatalogProvider.refresh_memberships
      end
    end

    assert_equal [ BOT_USER_ID, "U_OLD" ], channel.reload.member_user_ids
    assert_match(/omitted the bot user/, channel.membership_error)
    client.verify
  end

  test "rate limits preserve membership and remain retryable" do
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

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        error = assert_raises(SlackApi::RetryableError) do
          SlackChannelCatalogProvider.refresh_memberships
        end
        assert_equal 12, error.retry_after
      end
    end

    channel.reload
    assert_equal [ BOT_USER_ID, "U_OLD" ], channel.member_user_ids
    assert_nil channel.membership_last_attempted_at
  end

  test "membership reconciliation is bounded to the configured batch size" do
    (SlackChannelCatalogProvider::MEMBERSHIP_BATCH_SIZE + 1).times do |index|
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

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        assert_equal(
          SlackChannelCatalogProvider::MEMBERSHIP_BATCH_SIZE,
          SlackChannelCatalogProvider.refresh_memberships
        )
      end
    end

    assert_equal SlackChannelCatalogProvider::MEMBERSHIP_BATCH_SIZE, refreshed.length
  end

  test "a token change cannot read rows from the previous catalog configuration" do
    create_channel(channel_id: "C0123456789", name: "general")

    with_catalog do
      assert_equal [ "C0123456789" ], SlackChannelCatalogProvider.fetch.channels.map(&:id)
    end

    with_env(
      "CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-replacement-token",
      "SLACK_BOT_TOKEN" => nil,
      "SLACK_API_URL" => API_URL
    ) do
      Rails.stub(:cache, @cache) do
        result = SlackChannelCatalogProvider.fetch
        assert_empty result.channels
        assert_match(/loading/, result.error)
      end
    end
  end

  test "targeted refresh discovers a newly used channel before loading members" do
    client = mock_client
    client.expect(:fetch_identity, identity)
    client.expect(:fetch_channel, remote_channel(id: "C0123456789", name: "general"), [ "C0123456789" ])
    client.expect(:fetch_member_user_ids, [ BOT_USER_ID, "U1111111111" ], [ "C0123456789" ])

    with_catalog do
      SlackChannelCatalog.stub(:new, ->(**) { client }) do
        SlackChannelCatalogProvider.refresh_memberships(channel_id: "C0123456789")
      end
    end

    channel = SlackBotChannel.find_by!(channel_id: "C0123456789")
    assert_equal "general", channel.name
    assert_equal [ BOT_USER_ID, "U1111111111" ], channel.member_user_ids
    client.verify
  end

  test "unconfigured provider does not enqueue" do
    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => nil, "SLACK_BOT_TOKEN" => nil) do
      assert_no_enqueued_jobs do
        result = SlackChannelCatalogProvider.fetch
        refute result.configured
        assert_match(/not configured/, result.error)
      end
    end
  end

  private

  def with_catalog(&)
    with_env(
      "CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => TOKEN,
      "SLACK_BOT_TOKEN" => nil,
      "SLACK_API_URL" => API_URL
    ) do
      Rails.stub(:cache, @cache, &)
    end
  end

  def digest
    key = SlackChannelCatalogProvider.cache_key(token: TOKEN, api_url: API_URL)
    Digest::SHA256.hexdigest(key)
  end

  def create_channel(channel_id:, name:, private: false, member_user_ids: [ BOT_USER_ID ],
                     membership_refreshed_at: Time.current)
    SlackBotChannel.create!(
      configuration_digest: digest,
      team_id: TEAM_ID,
      bot_user_id: BOT_USER_ID,
      channel_id: channel_id,
      name: name,
      private: private,
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
