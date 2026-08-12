require "test_helper"

class CentaurApiClientTest < ActiveSupport::TestCase
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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    assert_equal({ "imports" => [] }, client.list_slack_archive_imports(limit: 25))
    http.verify
  end

  test "creates Slack archive imports with optional bearer auth" do
    http = Minitest::Mock.new
    expect_request(http, status: 201, body: { ok: true }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "Bearer secret-key", request[:headers]["Authorization"]
      body = JSON.parse(request[:body])
      assert_equal "export.zip", body["filename"]
      assert_equal({ "source" => "test" }, body["metadata"])
    end
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

    http.verify
  end

  test "raises useful errors for non-2xx responses" do
    http = Minitest::Mock.new
    expect_request(http, status: 400, body: { error: "bad archive" }.to_json)
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    error = assert_raises(CentaurApiClient::Error) do
      client.start_slack_archive_import("sai_bad")
    end
    http.verify
    assert_equal "bad archive", error.message
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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.ingest_slack_dm_sync_batch(run: { run_id: "sdms_1" }, messages: [])

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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

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

    http.verify
  end

  test "uses authenticated opaque development workflow contracts" do
    http = Minitest::Mock.new
    expect_request(http, status: 200, body: { items: [], next_cursor: nil }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal(
        "http://api.internal:8080/api/development/repositories?cursor=next%2Bpage&query=api+service",
        request[:url]
      )
      assert_equal "Bearer user-jwt", request[:headers]["Authorization"]
    end
    expect_request(http, status: 200, body: { thread_key: "development:1" }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/development/tasks", request[:url]
      body = JSON.parse(request[:body])
      assert_equal "web", body.dig("channel", "platform")
      assert_equal "usr_1", body.dig("initiator", "principal_id")
      assert_equal [ "gitlab:42", "gitlab:84" ], body["repository_ids"]
      assert_equal "Fix both services", body.dig("message", "parts", 0, "text")
      assert_nil body["clone_url"]
      assert_nil body["branch"]
      assert_nil body["role"]
    end
    expect_request(http, status: 200, body: { selection_flow_id: "sel_1" }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal(
        "http://api.internal:8080/api/development/sessions/development%3A1/repositories",
        request[:url]
      )
      assert_equal({ "requested_by_principal_id" => "usr_1" }, JSON.parse(request[:body]))
    end
    expect_request(http, status: 200, body: { workspace_id: "wsp_1", repositories: [] }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal(
        "http://api.internal:8080/api/development/sessions/development%3A1/repositories",
        request[:url]
      )
    end
    expect_request(http, status: 200, body: { state: "confirmed" }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal(
        "http://api.internal:8080/api/development/selections/sel_1/confirm",
        request[:url]
      )
      assert_equal(
        {
          "expected_version" => 1,
          "decided_by_principal_id" => "usr_1",
          "repository_ids" => [ "gitlab:84" ]
        },
        JSON.parse(request[:body])
      )
    end
    expect_request(http, status: 200, body: { changeset_id: "chg_1" }.to_json) do |request|
      assert_equal :get, request[:method]
      assert_equal "http://api.internal:8080/api/development/changesets/chg_1", request[:url]
    end
    expect_request(http, status: 200, body: { publish_batch_id: "pub_1" }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal "http://api.internal:8080/api/development/changesets/chg_1/publish", request[:url]
      assert_equal({ "idempotency_key" => "approve-1" }, JSON.parse(request[:body]))
    end
    expect_request(http, status: 200, body: { publish_batch_id: "pub_2" }.to_json) do |request|
      assert_equal :post, request[:method]
      assert_equal(
        "http://api.internal:8080/api/development/publish-batches/pub_1/retry",
        request[:url]
      )
      assert_equal({ "idempotency_key" => "retry-1" }, JSON.parse(request[:body]))
    end
    client = CentaurApiClient.new(
      base_url: "http://api.internal:8080",
      api_key: "user-jwt",
      http: http
    )

    client.search_development_repositories(query: "api service", cursor: "next+page")
    client.start_development_task(
      event_id: "evt-1",
      message_id: "msg-1",
      harness_type: "codex",
      principal_id: "usr_1",
      prompt: "Fix both services",
      session_metadata: { source: "console", model: "gpt-5.6-sol" },
      execution_metadata: { source: "console", model: "gpt-5.6-sol" },
      repository_ids: [ "gitlab:42", "gitlab:84" ]
    )
    client.create_add_repository_selection(thread_key: "development:1", principal_id: "usr_1")
    client.get_development_workspace("development:1")
    client.confirm_development_selection(
      selection_flow_id: "sel_1",
      expected_version: 1,
      principal_id: "usr_1",
      repository_ids: [ "gitlab:84" ]
    )
    client.get_development_changeset("chg_1")
    client.approve_development_changeset("chg_1", idempotency_key: "approve-1")
    client.retry_development_publish_batch("pub_1", idempotency_key: "retry-1")

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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

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
    client = CentaurApiClient.new(base_url: "http://api.internal:8080", http: http)

    client.create_workflow_run(workflow_name: "slack_sync")
    client.create_workflow_run(workflow_name: "slack_sync", input: { "mode" => "full" })

    http.verify
  end
end
