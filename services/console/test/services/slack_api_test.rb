require "test_helper"

class SlackApiTest < ActiveSupport::TestCase
  test "rate limits expose a bounded retry delay" do
    response = HttpClient::Response.new(
      status: 429,
      body: "",
      headers: { "retry-after" => "900" }
    )

    error = assert_raises(SlackApi::RateLimitedError) do
      SlackApi.parse_response!(response, operation: "conversations.members")
    end

    assert_equal SlackApi::DEFAULT_MAX_RATE_LIMIT_WAIT_SECONDS, error.retry_after
    assert_match "conversations.members", error.message
  end

  test "transient Slack errors expose a retry delay" do
    response = HttpClient::Response.new(
      status: 200,
      body: { ok: false, error: "internal_error" }.to_json
    )

    error = assert_raises(SlackApi::TransientError) do
      SlackApi.parse_response!(response)
    end

    assert_equal SlackApi::DEFAULT_TRANSIENT_RETRY_AFTER_SECONDS, error.retry_after
  end

  test "non-retryable Slack errors use the shared API error" do
    response = HttpClient::Response.new(
      status: 200,
      body: { ok: false, error: "missing_scope" }.to_json
    )

    error = assert_raises(SlackApi::Error) { SlackApi.parse_response!(response) }

    assert_match "missing_scope", error.message
    assert_equal "missing_scope", error.code
  end
end
