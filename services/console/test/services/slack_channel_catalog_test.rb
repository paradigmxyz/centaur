require "test_helper"

class SlackChannelCatalogTest < ActiveSupport::TestCase
  test "fetch caches slack channel catalog lookups" do
    cache = ActiveSupport::Cache::MemoryStore.new
    result = SlackChannelCatalog::Result.new(
      channels: [ SlackChannelCatalog::Channel.new(id: "C0123456789", name: "general", private: false) ],
      error: nil,
      configured: true
    )
    catalog = Minitest::Mock.new
    catalog.expect(:fetch, result)

    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", "SLACK_API_URL" => "https://slack.test/api") do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(token:, api_url:) { catalog }) do
          first = SlackChannelCatalog.fetch
          second = SlackChannelCatalog.fetch

          assert_equal [ "C0123456789" ], first.channels.map(&:id)
          assert_equal [ "C0123456789" ], second.channels.map(&:id)
          catalog.verify
        end
      end
    end
  end

  test "fetch does not cache slack channel catalog errors" do
    cache = ActiveSupport::Cache::MemoryStore.new
    error = SlackChannelCatalog::Result.new(channels: [], error: "Slack API request failed.", configured: true)
    success = SlackChannelCatalog::Result.new(
      channels: [ SlackChannelCatalog::Channel.new(id: "C0123456789", name: "general", private: false) ],
      error: nil,
      configured: true
    )
    catalog = Minitest::Mock.new
    catalog.expect(:fetch, error)
    catalog.expect(:fetch, success)

    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", "SLACK_API_URL" => "https://slack.test/api") do
      Rails.stub(:cache, cache) do
        SlackChannelCatalog.stub(:new, ->(token:, api_url:) { catalog }) do
          first = SlackChannelCatalog.fetch
          second = SlackChannelCatalog.fetch
          third = SlackChannelCatalog.fetch

          assert_equal "Slack API request failed.", first.error
          assert_nil second.error
          assert_nil third.error
          catalog.verify
        end
      end
    end
  end

  test "fetch configures short slack api timeouts" do
    api = Minitest::Mock.new
    api.expect(:get, HttpClient::Response.new(status: 200, body: { ok: true, channels: [] }.to_json),
               [ "https://slack.test/api/conversations.list" ],
               params: { types: SlackChannelCatalog::DEFAULT_TYPES, exclude_archived: "true", limit: "1000" },
               headers: { "Authorization" => "Bearer xoxb-test-token" })
    captured_options = nil
    factory = lambda do |**options|
      captured_options = options
      api
    end

    HttpClient.stub(:new, factory) do
      result = SlackChannelCatalog.new(token: "xoxb-test-token", api_url: "https://slack.test/api").fetch

      assert_predicate result, :ok?
    end

    assert_equal SlackChannelCatalog::OPEN_TIMEOUT_SECONDS, captured_options.fetch(:open_timeout)
    assert_equal SlackChannelCatalog::READ_TIMEOUT_SECONDS, captured_options.fetch(:read_timeout)
    assert_equal SlackChannelCatalog::WRITE_TIMEOUT_SECONDS, captured_options.fetch(:write_timeout)
    api.verify
  end
end
