require "test_helper"

class SlackChannelCatalogTest < ActiveSupport::TestCase
  TOKEN = "xoxb-test-token"
  API_URL = "https://slack.test/api"
  HEADERS = { "Authorization" => "Bearer #{TOKEN}" }.freeze

  test "fetch lists only channel conversations and configures short timeouts" do
    api = Minitest::Mock.new
    api.expect(
      :get,
      response(ok: true, channels: [
        { id: "C0123456789", name: "general", is_private: false },
        { id: "D0123456789", user: "U0123456789", is_im: true }
      ]),
      [ "#{API_URL}/users.conversations" ],
      params: { types: SlackChannelCatalog::DEFAULT_TYPES, exclude_archived: "false", limit: "200" },
      headers: HEADERS
    )
    captured_options = nil

    HttpClient.stub(:new, ->(**options) { captured_options = options; api }) do
      result = SlackChannelCatalog.new(token: TOKEN, api_url: API_URL).fetch

      assert_predicate result, :ok?
      assert_equal [ "C0123456789" ], result.channels.map(&:id)
    end

    assert_equal SlackChannelCatalog::OPEN_TIMEOUT_SECONDS, captured_options.fetch(:open_timeout)
    assert_equal SlackChannelCatalog::READ_TIMEOUT_SECONDS, captured_options.fetch(:read_timeout)
    assert_equal SlackChannelCatalog::WRITE_TIMEOUT_SECONDS, captured_options.fetch(:write_timeout)
    api.verify
  end

  test "identity and membership requests fully paginate" do
    api = Minitest::Mock.new
    api.expect(:get, response(ok: true, team_id: "T0123456789", user_id: "U0999999999"),
               [ "#{API_URL}/auth.test" ], params: {}, headers: HEADERS)
    api.expect(:get, response(ok: true, members: %w[U0999999999 U1111111111],
                              response_metadata: { next_cursor: "next" }),
               [ "#{API_URL}/conversations.members" ],
               params: { channel: "C0123456789", limit: "200" }, headers: HEADERS)
    api.expect(:get, response(ok: true, members: %w[U2222222222 U1111111111],
                              response_metadata: { next_cursor: "" }),
               [ "#{API_URL}/conversations.members" ],
               params: { channel: "C0123456789", limit: "200", cursor: "next" }, headers: HEADERS)
    catalog = SlackChannelCatalog.new(token: TOKEN, api_url: API_URL, api: api)

    identity = catalog.fetch_identity
    members = catalog.fetch_member_user_ids("C0123456789")

    assert_equal "T0123456789", identity.team_id
    assert_equal "U0999999999", identity.bot_user_id
    assert_equal %w[U0999999999 U1111111111 U2222222222], members
    api.verify
  end

  test "rate limits use the shared Slack API error" do
    api = Minitest::Mock.new
    api.expect(:get, HttpClient::Response.new(status: 429, body: "", headers: { "retry-after" => "900" }),
               [ "#{API_URL}/conversations.members" ],
               params: { channel: "C0123456789", limit: "200" }, headers: HEADERS)
    catalog = SlackChannelCatalog.new(token: TOKEN, api_url: API_URL, api: api)

    error = assert_raises(SlackApi::RateLimitedError) do
      catalog.fetch_member_user_ids("C0123456789")
    end

    assert_equal SlackApi::DEFAULT_MAX_RATE_LIMIT_WAIT_SECONDS, error.retry_after
    api.verify
  end

  private

  def response(payload)
    HttpClient::Response.new(status: 200, body: payload.to_json)
  end
end
