require "cgi"
require "uri"

class CentaurApiClient
  Error = Class.new(StandardError)

  DEFAULT_TIMEOUT_SECONDS = 20

  attr_reader :base_url

  def initialize(base_url: nil, token_provider: -> { ApiServer::Jwt.encode_for_console_service },
                 http: nil, timeout: DEFAULT_TIMEOUT_SECONDS, read_timeout: timeout)
    @base_url = (base_url.presence || ConsoleEnv["CENTAUR_API_URL"].presence || "http://localhost:8080").delete_suffix("/")
    @token_provider = token_provider
    @api = HttpClient.new(http: http, open_timeout: timeout, read_timeout: read_timeout)
  end

  def list_slack_archive_imports(limit: 100)
    get("/api/admin/slack/archive-imports", limit: limit)
  end

  def create_slack_archive_import(filename:, content_type:, created_by:, metadata: {})
    post(
      "/api/admin/slack/archive-imports",
      {
        filename: filename,
        content_type: content_type,
        created_by: created_by,
        metadata: metadata
      }
    )
  end

  def start_slack_archive_import(import_id)
    post("/api/admin/slack/archive-imports/#{escape_path(import_id)}/start", {})
  end

  def retry_slack_archive_import(import_id)
    post("/api/admin/slack/archive-imports/#{escape_path(import_id)}/retry", {})
  end

  def delete_slack_archive_import(import_id)
    request(:delete, "/api/admin/slack/archive-imports/#{escape_path(import_id)}")
  end

  def list_slack_dm_sync_checkpoints(broker_credential_id:, home_team_id: nil)
    get(
      "/api/admin/slack/dm-sync/checkpoints",
      broker_credential_id: broker_credential_id,
      home_team_id: home_team_id
    )
  end

  def ingest_slack_dm_sync_batch(payload)
    post("/api/admin/slack/dm-sync/batch", payload)
  end

  def get_google_docs_sync_checkpoint(broker_credential_id:)
    get(
      "/api/admin/google/docs-sync/checkpoint",
      broker_credential_id: broker_credential_id
    )
  end

  def ingest_google_docs_sync_batch(payload)
    post("/api/admin/google/docs-sync/batch", payload)
  end

  def get_granola_sync_checkpoint(scope_id:)
    get("/api/admin/granola/sync/checkpoint", scope_id: scope_id)
  end

  def ingest_granola_sync_batch(payload)
    post("/api/admin/granola/sync/batch", payload)
  end

  def create_session(thread_key:, harness_type:, metadata: {}, persona_id: nil,
                     on_harness_conflict: "reject")
    payload = {
      harness_type: harness_type,
      metadata: metadata,
      on_harness_conflict: on_harness_conflict
    }
    payload[:persona_id] = persona_id if persona_id.present?

    post("/api/session/#{escape_path(thread_key)}", payload)
  end

  def append_session_messages(thread_key:, messages:)
    post("/api/session/#{escape_path(thread_key)}/messages", { messages: messages })
  end

  def execute_session(thread_key:, input_lines:, idempotency_key: nil, metadata: {})
    payload = {
      input_lines: input_lines,
      metadata: metadata
    }
    payload[:idempotency_key] = idempotency_key if idempotency_key.present?

    post("/api/session/#{escape_path(thread_key)}/execute", payload)
  end

  def list_workflow_schedules
    get("/api/workflows/schedules")
  end

  def get_workflow_run(run_id)
    get("/api/workflows/runs/#{escape_path(run_id)}")
  end

  def create_workflow_run(workflow_name:, input: nil)
    payload = { workflow_name: workflow_name }
    payload[:input] = input unless input.nil?

    post("/api/workflows/runs", payload)
  end

  private

  def get(path, params = {})
    query = params.compact.to_query
    request(:get, query.present? ? "#{path}?#{query}" : path)
  end

  def post(path, payload)
    request(:post, path, payload)
  end

  def request(method, path, payload = nil)
    response = @api.request(
      method: method,
      url: URI.join("#{@base_url}/", path.delete_prefix("/")).to_s,
      json: payload,
      headers: request_headers
    )
    parsed = parse_body(response.body)
    return parsed if response.status.between?(200, 299)

    message = parsed.is_a?(Hash) ? parsed["error"] || parsed["message"] || parsed["detail"] : nil
    raise Error, message.presence || "Centaur API returned HTTP #{response.status}"
  end

  def request_headers
    token = @token_provider.call
    raise Error, "Console API service JWT could not be minted" if token.blank?

    headers = { "Accept" => "application/json" }
    headers["Content-Type"] = "application/json"
    headers["Authorization"] = "Bearer #{token}"
    headers
  end

  def parse_body(body)
    HttpClient.decode_json_body(body)
  rescue JSON::ParserError
    { "raw" => body.to_s }
  end

  def escape_path(value)
    CGI.escape(value.to_s)
  end
end
