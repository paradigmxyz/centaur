require "test_helper"

class SlackChannelCatalogTest < ActiveSupport::TestCase
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
