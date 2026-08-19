require "test_helper"

class CentaurApiClientTest < ActiveSupport::TestCase
  def api_client(**options)
    CentaurApiClient.new(token_provider: -> { "console-service-jwt" }, **options)
  end

  def expect_request(http, status:, body:)
    expect_http_call(http, status: status, body: body) { |request| yield request if block_given? }
  end

  test "lists Slack archive imports with query params" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { imports: [] }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal "http://api.internal:8080/api/admin/slack/archive-imports?limit=25", request[:url]
      assert_equal "application/json", request[:headers]["Accept"]
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    assert_equal({ "imports" => [] }, client.list_slack_archive_imports(limit: 25))
    http.verify
  end

  test "creates Slack archive imports with a Console service JWT" do
    http = Minitest::Mock.new
    expect_request(http, status: 201, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "Bearer console-service-jwt", request[:headers]["Authorization"]
      body = JSON.parse(request[:body])
      assert_equal "export.zip", body["filename"]
      assert_equal({ "source" => "test" }, body["metadata"])
    end
    client = api_client(
      base_url: "http://api.internal:8080/",
      http: http
    )

    client.create_slack_archive_import(
      filename: "export.zip",
      content_type: "application/zip",
      created_by: "admin@example.com",
      metadata: { source: "test" }
    )

    http.verify
  end

  test "raises useful errors for non-2xx responses" do
    http = Minitest::Mock.new
    expect_request(http, status: 400, body: { error: "bad archive" }.to_json)
    client = api_client(base_url: "http://api.internal:8080", http: http)

    error = assert_raises(CentaurApiClient::Error) do
      client.start_slack_archive_import("sai_bad")
    end
    http.verify
    assert_equal "bad archive", error.message
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
  end

  test "lists Slack DM sync checkpoints for a broker credential" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { checkpoints: [] }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal(
        "http://api.internal:8080/api/admin/slack/dm-sync/checkpoints?broker_credential_id=bcr_123&home_team_id=T123",
        request[:url]
      )
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.list_slack_dm_sync_checkpoints(
      broker_credential_id: "bcr_123",
      home_team_id: "T123"
    )

    http.verify
  end

  test "posts Slack DM sync batches" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/admin/slack/dm-sync/batch", request[:url]
      assert_equal({ "run" => { "run_id" => "sdms_1" }, "messages" => [] }, JSON.parse(request[:body]))
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.ingest_slack_dm_sync_batch(run: { run_id: "sdms_1" }, messages: [])

    http.verify
  end

  test "supports a longer read timeout for sync batches" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal 120, request[:timeout]
    end
    client = api_client(
      base_url: "http://api.internal:8080",
      http: http,
      read_timeout: 120
    )

    client.ingest_slack_dm_sync_batch(messages: [])

    http.verify
  end

  test "gets Google Docs sync checkpoint for a broker credential" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { checkpoint: nil }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal(
        "http://api.internal:8080/api/admin/google/docs-sync/checkpoint?broker_credential_id=bcr_123",
        request[:url]
      )
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.get_google_docs_sync_checkpoint(broker_credential_id: "bcr_123")

    http.verify
  end

  test "posts Google Docs sync batches" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/admin/google/docs-sync/batch", request[:url]
      assert_equal({ "run" => { "run_id" => "gdocs_1" }, "files" => [] }, JSON.parse(request[:body]))
    end
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.ingest_google_docs_sync_batch(run: { run_id: "gdocs_1" }, files: [])

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

  test "creates workflow runs with optional input" do
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
    client = api_client(base_url: "http://api.internal:8080", http: http)

    client.create_workflow_run(workflow_name: "slack_sync")
    client.create_workflow_run(workflow_name: "slack_sync", input: { "mode" => "full" })

    http.verify
  end
end
