require "test_helper"

class CentaurApiClientTest < ActiveSupport::TestCase
  def api_client(**options)
    CentaurApiClient.new(token_provider: -> { "console-service-jwt" }, **options)
  end

  def expect_request(http, status:, body:)
    expect_http_call(http, status: status, body: body) { |request| yield request if block_given? }
  end

  test "raises useful errors for non-2xx responses" do
    http = Minitest::Mock.new
    expect_request(http, status: 400, body: { error: "bad run" }.to_json)
    client = api_client(base_url: "http://api.internal:8080", http: http)

    error = assert_raises(CentaurApiClient::Error) do
      client.get_workflow_run("run_bad")
    end
    http.verify
    assert_equal "bad run", error.message
    assert_equal 400, error.status
  end

  test "fails before the request when the service JWT cannot be minted" do
    http = Minitest::Mock.new
    client = CentaurApiClient.new(
      base_url: "http://api.internal:8080",
      token_provider: -> { nil },
      http: http
    )

    error = assert_raises(CentaurApiClient::Error) do
      client.list_workflow_schedules
    end

    assert_equal "Console API service JWT could not be minted", error.message
    assert_nil error.status
  end

  test "replaces Slack external team ids" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { slack_external_team_ids: [ "T0PARTNER" ] }.to_json) do |request|
      assert_equal :put, request[:method]
      assert_equal "http://api.internal:8080/api/slack/external-team-ids", request[:url]
      assert_equal({ "slack_external_team_ids" => [ "t0partner" ] }, JSON.parse(request[:body]))
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    assert_equal [ "T0PARTNER" ], client.replace_slack_external_team_ids([ "t0partner" ])
    http.verify
  end

  test "supports a longer read timeout" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal 120, request[:timeout]
    end
    client = api_client(
      base_url: "http://api.internal:8080",
      http: http,
      read_timeout: 120
    )

    client.list_workflow_schedules

    http.verify
  end

  test "creates app sessions with encoded thread keys" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/session/console%3Aabc-123", request[:url]
      body = JSON.parse(request[:body])
      assert_equal "codex", body["harness_type"]
      assert_equal({ "source" => "console" }, body["metadata"])
      assert_equal "reject", body["on_harness_conflict"]
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.create_session(
      thread_key: "console:abc-123",
      harness_type: "codex",
      metadata: { source: "console" },
      on_harness_conflict: "reject"
    )

    http.verify
  end

  test "appends and executes app session messages" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/session/console%3Aabc-123/messages", request[:url]
      assert_equal "user", JSON.parse(request[:body]).dig("messages", 0, "role")
    end
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/session/console%3Aabc-123/execute", request[:url]
      body = JSON.parse(request[:body])
      assert_equal [ '{"type":"user"}' ], body["input_lines"]
      assert_equal "idem-1", body["idempotency_key"]
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.append_session_messages(
      thread_key: "console:abc-123",
      messages: [ { role: "user", parts: [ { type: "text", text: "hi" } ] } ]
    )
    client.execute_session(
      thread_key: "console:abc-123",
      input_lines: [ '{"type":"user"}' ],
      idempotency_key: "idem-1",
      metadata: { source: "console" }
    )

    http.verify
  end

  test "lists workflow schedules and fetches run details" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true, schedules: [] }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal "http://api.internal:8080/api/workflows/schedules", request[:url]
    end
    expect_request(http, status: 200, body: { ok: true, schedules: [] }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal "http://api.internal:8080/api/workflows/runs/run%3A1", request[:url]
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.list_workflow_schedules
    client.get_workflow_run("run:1")

    http.verify
  end

  test "creates workflow runs with optional input and idempotency key" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true, run_id: "r1" }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/workflows/runs", request[:url]
      assert_equal({ "workflow_name" => "slack_sync" }, JSON.parse(request[:body]))
    end
    expect_request(http, status: 200, body: { ok: true, run_id: "r1" }.to_json) do |request|
      assert_equal(
        { "workflow_name" => "slack_sync", "input" => { "mode" => "full" } },
        JSON.parse(request[:body])
      )
    end
    expect_request(http, status: 200, body: { ok: true, run_id: "r1" }.to_json) do |request|
      assert_equal(
        {
          "workflow_name" => "console_workflow",
          "idempotency_key" => "scheduled:1",
          "max_attempts" => 3
        },
        JSON.parse(request[:body])
      )
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.create_workflow_run(workflow_name: "slack_sync")
    client.create_workflow_run(workflow_name: "slack_sync", input: { "mode" => "full" })
    client.create_workflow_run(
      workflow_name: "console_workflow",
      idempotency_key: "scheduled:1",
      max_attempts: 3
    )

    http.verify
  end

  test "runs a scheduled task for an optional occurrence" do
    http = Minitest::Mock.new
    expect_request(http, status: 202, body: { ok: true, run_id: "r1" }.to_json) do |request|
      assert_equal "http://api.internal:8080/api/scheduled-tasks/tsk_abc/runs", request[:url]
      assert_equal({}, JSON.parse(request[:body]))
    end
    expect_request(http, status: 202, body: { ok: true, run_id: "r2" }.to_json) do |request|
      assert_equal({ "scheduled_for" => "2026-08-19T12:00:00Z" }, JSON.parse(request[:body]))
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    assert_equal "r1", client.run_scheduled_task("tsk_abc")["run_id"]
    assert_equal "r2", client.run_scheduled_task("tsk_abc", scheduled_for: Time.utc(2026, 8, 19, 12))["run_id"]

    http.verify
  end
end
