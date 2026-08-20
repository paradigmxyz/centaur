require "test_helper"

class SlackChannelCatalogProviderTest < ActiveJob::TestCase
  TOKEN = "xoxb-test-token"
  TEAM_ID = "T0123456789"
  BOT_USER_ID = "U0999999999"

  setup do
    SlackBotChannel.delete_all
    @cache = ActiveSupport::Cache::MemoryStore.new
  end

  test "cold reads enqueue one channel sync" do
    with_catalog do
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

  test "reads return durable rows without enqueueing Slack work" do
    create_channel(channel_id: "C0123456789", name: "general", last_seen_at: 2.days.ago)

    with_catalog do
      assert_no_enqueued_jobs do
        result = SlackChannelCatalogProvider.fetch
        assert_equal [ "C0123456789" ], result.channels.map(&:id)
        assert_nil result.error
      end
    end
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

  test "names include inactive channels already referenced by permissions" do
    create_channel(channel_id: "C0123456789", name: "former-channel", active: false)

    with_catalog do
      assert_equal(
        { "C0123456789" => "former-channel" },
        SlackChannelCatalogProvider.names_for([ "C0123456789" ])
      )
    end
  end

  test "a token rotation continues serving the durable catalog" do
    create_channel(channel_id: "C0123456789", name: "general")

    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-replacement-token") do
      Rails.stub(:cache, @cache) do
        result = SlackChannelCatalogProvider.fetch
        assert_equal [ "C0123456789" ], result.channels.map(&:id)
        assert_nil result.error
      end
    end
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
    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => TOKEN, "SLACK_BOT_TOKEN" => nil) do
      Rails.stub(:cache, @cache, &)
    end
  end

  def create_channel(channel_id:, name:, private: false, active: true,
                     member_user_ids: [ BOT_USER_ID ], last_seen_at: Time.current)
    SlackBotChannel.create!(
      team_id: TEAM_ID,
      bot_user_id: BOT_USER_ID,
      channel_id: channel_id,
      name: name,
      private: private,
      archived: false,
      active: active,
      member_user_ids: member_user_ids,
      membership_refreshed_at: Time.current,
      last_seen_at: last_seen_at
    )
  end
end
