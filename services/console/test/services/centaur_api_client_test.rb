require "test_helper"

class CentaurApiClientTest < ActiveSupport::TestCase
  class StubHTTP
    attr_reader :requests

    def initialize(status:, body:)
      @status = status
      @body = body
      @requests = []
    end

    def call(method:, url:, body:, headers:, timeout:)
      @requests << { method: method, url: url, body: body, headers: headers, timeout: timeout }
      CentaurApiClient::Response.new(status: @status, body: @body)
    end
  end

  test "lists Slack archive imports with query params" do
    http = StubHTTP.new(status: 200, body: { imports: [] }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    assert_equal({ "imports" => [] }, client.list_slack_archive_imports(limit: 25))
    request = http.requests.first
    assert_equal :get, request[:method]
    assert_equal "http://api.internal:8080/api/admin/slack/archive-imports?limit=25", request[:url]
    assert_equal "application/json", request[:headers]["Accept"]
  end

  test "creates Slack archive imports with optional bearer auth" do
    http = StubHTTP.new(status: 201, body: { ok: true }.to_json)
    client = CentaurApiClient.new(
      base_url: "http://api.internal:8080/",
      api_key: "secret-key",
      http: http
    )

    client.create_slack_archive_import(
      filename: "export.zip",
      content_type: "application/zip",
      created_by: "admin@example.com",
      metadata: { source: "test" }
    )

    request = http.requests.first
    assert_equal :post, request[:method]
    assert_equal "Bearer secret-key", request[:headers]["Authorization"]
    body = JSON.parse(request[:body])
    assert_equal "export.zip", body["filename"]
    assert_equal({ "source" => "test" }, body["metadata"])
  end

  test "raises useful errors for non-2xx responses" do
    http = StubHTTP.new(status: 400, body: { error: "bad archive" }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    error = assert_raises(CentaurApiClient::Error) do
      client.start_slack_archive_import("sai_bad")
    end
    assert_equal "bad archive", error.message
  end

  test "lists Slack DM sync checkpoints for a broker credential" do
    http = StubHTTP.new(status: 200, body: { checkpoints: [] }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.list_slack_dm_sync_checkpoints(
      broker_credential_id: "bcr_123",
      home_team_id: "T123"
    )

    request = http.requests.first
    assert_equal :get, request[:method]
    assert_equal(
      "http://api.internal:8080/api/admin/slack/dm-sync/checkpoints?broker_credential_id=bcr_123&home_team_id=T123",
      request[:url]
    )
  end

  test "posts Slack DM sync batches" do
    http = StubHTTP.new(status: 200, body: { ok: true }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.ingest_slack_dm_sync_batch(run: { run_id: "sdms_1" }, messages: [])

    request = http.requests.first
    assert_equal :post, request[:method]
    assert_equal "http://api.internal:8080/api/admin/slack/dm-sync/batch", request[:url]
    assert_equal({ "run" => { "run_id" => "sdms_1" }, "messages" => [] }, JSON.parse(request[:body]))
  end

  test "gets Google Docs sync checkpoint for a broker credential" do
    http = StubHTTP.new(status: 200, body: { checkpoint: nil }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.get_google_docs_sync_checkpoint(broker_credential_id: "bcr_123")

    request = http.requests.first
    assert_equal :get, request[:method]
    assert_equal(
      "http://api.internal:8080/api/admin/google/docs-sync/checkpoint?broker_credential_id=bcr_123",
      request[:url]
    )
  end

  test "posts Google Docs sync batches" do
    http = StubHTTP.new(status: 200, body: { ok: true }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.ingest_google_docs_sync_batch(run: { run_id: "gdocs_1" }, files: [])

    request = http.requests.first
    assert_equal :post, request[:method]
    assert_equal "http://api.internal:8080/api/admin/google/docs-sync/batch", request[:url]
    assert_equal({ "run" => { "run_id" => "gdocs_1" }, "files" => [] }, JSON.parse(request[:body]))
  end

  test "creates app sessions with encoded thread keys" do
    http = StubHTTP.new(status: 200, body: { ok: true }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.create_session(
      thread_key: "console:abc-123",
      harness_type: "codex",
      metadata: { source: "console" },
      on_harness_conflict: "reject"
    )

    request = http.requests.first
    assert_equal :post, request[:method]
    assert_equal "http://api.internal:8080/api/session/console%3Aabc-123", request[:url]
    body = JSON.parse(request[:body])
    assert_equal "codex", body["harness_type"]
    assert_equal({ "source" => "console" }, body["metadata"])
    assert_equal "reject", body["on_harness_conflict"]
  end

  test "appends and executes app session messages" do
    http = StubHTTP.new(status: 200, body: { ok: true }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

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

    append = http.requests.first
    assert_equal :post, append[:method]
    assert_equal "http://api.internal:8080/api/session/console%3Aabc-123/messages", append[:url]
    assert_equal "user", JSON.parse(append[:body]).dig("messages", 0, "role")

    execute = http.requests.second
    assert_equal :post, execute[:method]
    assert_equal "http://api.internal:8080/api/session/console%3Aabc-123/execute", execute[:url]
    body = JSON.parse(execute[:body])
    assert_equal [ '{"type":"user"}' ], body["input_lines"]
    assert_equal "idem-1", body["idempotency_key"]
  end

  test "lists workflow schedules and fetches run details" do
    http = StubHTTP.new(status: 200, body: { ok: true, schedules: [] }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.list_workflow_schedules
    client.get_workflow_run("run:1")

    schedules = http.requests.first
    assert_equal :get, schedules[:method]
    assert_equal "http://api.internal:8080/api/workflows/schedules", schedules[:url]

    run = http.requests.second
    assert_equal :get, run[:method]
    assert_equal "http://api.internal:8080/api/workflows/runs/run%3A1", run[:url]
  end

  test "creates workflow runs with optional input" do
    http = StubHTTP.new(status: 200, body: { ok: true, run_id: "r1" }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.create_workflow_run(workflow_name: "slack_sync")
    client.create_workflow_run(workflow_name: "slack_sync", input: { "mode" => "full" })

    bare = http.requests.first
    assert_equal :post, bare[:method]
    assert_equal "http://api.internal:8080/api/workflows/runs", bare[:url]
    assert_equal({ "workflow_name" => "slack_sync" }, JSON.parse(bare[:body]))

    with_input = http.requests.second
    assert_equal({ "workflow_name" => "slack_sync", "input" => { "mode" => "full" } }, JSON.parse(with_input[:body]))
  end
end
