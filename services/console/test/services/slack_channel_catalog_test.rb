require "test_helper"

class SlackChannelCatalogTest < ActiveSupport::TestCase
  test "fetch caches slack channel catalog lookups" do
    cache = ActiveSupport::Cache::MemoryStore.new
    calls = 0
    result = SlackChannelCatalog::Result.new(
      channels: [ SlackChannelCatalog::Channel.new(id: "C0123456789", name: "general", private: false) ],
      error: nil,
      configured: true
    )
    catalog = Object.new
    catalog.define_singleton_method(:fetch) do
      calls += 1
      result
    end

    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", "SLACK_API_URL" => "https://slack.test/api") do
      with_singleton_method(Rails, :cache, -> { cache }) do
        with_singleton_method(SlackChannelCatalog, :new, ->(token:, api_url:) { catalog }) do
          first = SlackChannelCatalog.fetch
          second = SlackChannelCatalog.fetch

          assert_equal 1, calls
          assert_equal [ "C0123456789" ], first.channels.map(&:id)
          assert_equal [ "C0123456789" ], second.channels.map(&:id)
        end
      end
    end
  end

  test "fetch does not cache slack channel catalog errors" do
    cache = ActiveSupport::Cache::MemoryStore.new
    calls = 0
    error = SlackChannelCatalog::Result.new(channels: [], error: "Slack API request failed.", configured: true)
    success = SlackChannelCatalog::Result.new(
      channels: [ SlackChannelCatalog::Channel.new(id: "C0123456789", name: "general", private: false) ],
      error: nil,
      configured: true
    )
    catalog = Object.new
    catalog.define_singleton_method(:fetch) do
      calls += 1
      calls == 1 ? error : success
    end

    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", "SLACK_API_URL" => "https://slack.test/api") do
      with_singleton_method(Rails, :cache, -> { cache }) do
        with_singleton_method(SlackChannelCatalog, :new, ->(token:, api_url:) { catalog }) do
          first = SlackChannelCatalog.fetch
          second = SlackChannelCatalog.fetch
          third = SlackChannelCatalog.fetch

          assert_equal "Slack API request failed.", first.error
          assert_nil second.error
          assert_nil third.error
          assert_equal 2, calls
        end
      end
    end
  end

  test "fetch configures short slack api timeouts" do
    response = Struct.new(:code, :body).new(
      "200",
      { ok: true, channels: [] }.to_json
    )
    http = Object.new
    http.define_singleton_method(:request) { |_request| response }
    captured_options = nil

    with_singleton_method(Net::HTTP, :start, lambda { |_host, _port, **options, &block|
      captured_options = options
      block.call(http)
    }) do
      result = SlackChannelCatalog.new(token: "xoxb-test-token", api_url: "https://slack.test/api").fetch

      assert_predicate result, :ok?
    end

    assert_equal SlackChannelCatalog::OPEN_TIMEOUT_SECONDS, captured_options.fetch(:open_timeout)
    assert_equal SlackChannelCatalog::READ_TIMEOUT_SECONDS, captured_options.fetch(:read_timeout)
    assert_equal SlackChannelCatalog::WRITE_TIMEOUT_SECONDS, captured_options.fetch(:write_timeout)
  end

  test "fetch includes one to one DMs in the channel catalog" do
    response = Struct.new(:code, :body).new(
      "200",
      {
        ok: true,
        channels: [
          { id: "C0123456789", name: "general", is_private: false },
          { id: "D9876543210", is_im: true, user: "U0123456789" }
        ]
      }.to_json
    )
    http = Object.new
    captured_request = nil
    http.define_singleton_method(:request) do |request|
      captured_request = request
      response
    end

    with_singleton_method(Net::HTTP, :start, lambda { |_host, _port, **_options, &block|
      block.call(http)
    }) do
      result = SlackChannelCatalog.new(token: "xoxb-test-token", api_url: "https://slack.test/api").fetch

      assert_predicate result, :ok?
      assert_includes captured_request.path, "types=public_channel%2Cprivate_channel%2Cim"
      assert_equal [ "C0123456789", "D9876543210" ], result.channels.map(&:id)
      dm = result.channels.find { |channel| channel.id == "D9876543210" }
      assert_equal "U0123456789", dm.name
      assert_equal true, dm.private
    end
  end

  test "channel_id_for_user resolves a Slack user id to its DM channel id" do
    result = SlackChannelCatalog::Result.new(
      channels: [
        SlackChannelCatalog::Channel.new(id: "D9876543210", name: "U0123456789", private: true)
      ],
      error: nil,
      configured: true
    )

    with_singleton_method(SlackChannelCatalog, :fetch, -> { result }) do
      assert_equal "D9876543210", SlackChannelCatalog.channel_id_for_user("u0123456789")
      assert_nil SlackChannelCatalog.channel_id_for_user("U9999999999")
    end
  end

  private

  def with_env(values)
    previous = values.keys.to_h { |key| [ key, ENV[key] ] }
    values.each { |key, value| value.nil? ? ENV.delete(key) : ENV[key] = value }
    yield
  ensure
    previous.each { |key, value| value.nil? ? ENV.delete(key) : ENV[key] = value }
  end

  def with_singleton_method(object, method_name, replacement)
    singleton = object.singleton_class
    original = singleton.instance_method(method_name)
    singleton.define_method(method_name, replacement)
    yield
  ensure
    singleton.define_method(method_name, original)
  end
end
