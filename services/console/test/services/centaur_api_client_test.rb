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
end
