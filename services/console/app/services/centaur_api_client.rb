require "cgi"
require "uri"

class CentaurApiClient
  Error = Class.new(StandardError)

  DEFAULT_TIMEOUT_SECONDS = 20

  attr_reader :base_url

  def initialize(base_url: nil, api_key: nil, http: nil, timeout: DEFAULT_TIMEOUT_SECONDS)
    @base_url = (base_url.presence || ConsoleEnv["CENTAUR_API_URL"].presence || "http://localhost:8080").delete_suffix("/")
    @api_key = api_key.presence || ConsoleEnv["CENTAUR_API_KEY"].presence
    @api = HttpClient.new(http: http, open_timeout: timeout, read_timeout: timeout)
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

  def search_development_repositories(query: nil, cursor: nil)
    get("/api/development/repositories", cursor: cursor, query: query)
  end

  def start_development_task(event_id:, message_id:, harness_type:, principal_id:, prompt:,
                             session_metadata:, execution_metadata:, repository_ids: nil)
    payload = {
      channel: {
        platform: "web",
        tenant_key: "console",
        conversation_key: principal_id,
        root_message_id: message_id
      },
      platform_event_id: event_id,
      platform_message_id: message_id,
      harness_type: harness_type,
      initiator: { principal_id: principal_id },
      message: {
        client_message_id: message_id,
        role: "user",
        parts: [ { type: "text", text: prompt } ],
        metadata: execution_metadata
      },
      session_metadata: session_metadata
    }
    payload[:repository_ids] = repository_ids unless repository_ids.nil?
    post("/api/development/tasks", payload)
  end

  def continue_development_task(channel:, event_id:, message_id:, principal_id:, prompt:, metadata:)
    post(
      "/api/development/tasks/continue",
      {
        channel: channel,
        platform_event_id: event_id,
        platform_message_id: message_id,
        sender_principal_id: principal_id,
        message: {
          client_message_id: message_id,
          role: "user",
          parts: [ { type: "text", text: prompt } ],
          metadata: metadata
        }
      }
    )
  end

  def create_add_repository_selection(thread_key:, principal_id:)
    post(
      "/api/development/sessions/#{escape_path(thread_key)}/repositories",
      { requested_by_principal_id: principal_id }
    )
  end

  def get_development_workspace(thread_key)
    get("/api/development/sessions/#{escape_path(thread_key)}/repositories")
  end

  def confirm_development_selection(selection_flow_id:, expected_version:, principal_id:, repository_ids:)
    post(
      "/api/development/selections/#{escape_path(selection_flow_id)}/confirm",
      {
        expected_version: expected_version,
        decided_by_principal_id: principal_id,
        repository_ids: repository_ids
      }
    )
  end

  def get_development_changeset(changeset_id)
    get("/api/development/changesets/#{escape_path(changeset_id)}")
  end

  def get_development_changeset_artifact(changeset_id, artifact_ref)
    request_raw(
      :get,
      "/api/development/changesets/#{escape_path(changeset_id)}/artifacts/#{escape_path(artifact_ref)}"
    )
  end

  def approve_development_changeset(changeset_id, idempotency_key:)
    post(
      "/api/development/changesets/#{escape_path(changeset_id)}/publish",
      { idempotency_key: idempotency_key }
    )
  end

  def get_development_publish_batch(publish_batch_id)
    get("/api/development/publish-batches/#{escape_path(publish_batch_id)}")
  end

  def retry_development_publish_batch(publish_batch_id, idempotency_key:)
    post(
      "/api/development/publish-batches/#{escape_path(publish_batch_id)}/retry",
      { idempotency_key: idempotency_key }
    )
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
    response = request_raw(method, path, payload)
    parsed = parse_body(response.body)
    return parsed if response.status.between?(200, 299)

    message = parsed.is_a?(Hash) ? parsed["error"] || parsed["message"] || parsed["detail"] : nil
    raise Error, message.presence || "Centaur API returned HTTP #{response.status}"
  end

  def request_raw(method, path, payload = nil)
    response = @api.request(
      method: method,
      url: URI.join("#{@base_url}/", path.delete_prefix("/")).to_s,
      json: payload,
      headers: request_headers
    )
    return response if response.status.between?(200, 299)

    parsed = parse_body(response.body)
    message = parsed.is_a?(Hash) ? parsed["error"] || parsed["message"] || parsed["detail"] : nil
    raise Error, message.presence || "Centaur API returned HTTP #{response.status}"
  end

  def request_headers
    headers = { "Accept" => "application/json" }
    headers["Content-Type"] = "application/json"
    headers["Authorization"] = "Bearer #{@api_key}" if @api_key.present?
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
