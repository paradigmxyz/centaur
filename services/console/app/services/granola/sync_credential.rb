require "cgi"
require "date"
require "json"
require "net/http"
require "time"
require "uri"

module Granola
  # Syncs one user's connected Granola MCP account. This runs in the console so
  # the OAuth access token never leaves the control plane; only normalized note
  # data is sent to api-rs for storage and RLS-protected access.
  class SyncCredential
    MCP_URL = "https://mcp.granola.ai/mcp"
    DEFAULT_INITIAL_LOOKBACK_DAYS = 365
    # The MCP service currently advertises an average limit of about 100
    # requests/minute. One run issues a list call, a batched detail call, and
    # at most one transcript call per note, so fifty keeps a normal run well
    # inside that envelope.
    DEFAULT_MAX_NOTES = 50
    WATERMARK_OVERLAP_SECONDS = 5 * 60

    MEETING_RE = /<meeting\s+id="(?<id>[^"]+)"\s+title="(?<title>[^"]*)"\s+date="(?<date>[^"]*)">(?<body>.*?)<\/meeting>/m
    PARTICIPANTS_RE = /<known_participants>(?<participants>.*?)<\/known_participants>/m
    SUMMARY_RE = /<summary>(?<summary>.*?)<\/summary>/m
    PARTICIPANT_RE = /(?<name>[^,<]+?)\s*<(?<email>[^>]+)>/
    MCP_DATE_RE = /\A(?<date>\w+ \d+, \d+ \d+:\d+ [AP]M) GMT(?<offset>[+-]\d+)?\z/

    GranolaApiError = Class.new(StandardError)

    class << self
      attr_accessor :mcp_http

      def oauth_app_slug
        ConsoleEnv["GRANOLA_SYNC_OAUTH_APP_SLUG"].presence || "granola"
      end

      def syncable?(credential, oauth_app_slug: self.oauth_app_slug)
        credential.present? && !credential.dead? && credential.access_token.present? &&
          credential.oauth_app&.provider == Oauth::Providers::Granola::KEY &&
          credential.oauth_app&.slug == oauth_app_slug && credential.oauth_app.enabled?
      end

      def initial_lookback_days
        positive_int(ConsoleEnv["GRANOLA_SYNC_INITIAL_LOOKBACK_DAYS"], DEFAULT_INITIAL_LOOKBACK_DAYS)
      end

      def max_notes
        positive_int(ConsoleEnv["GRANOLA_SYNC_MAX_NOTES"], DEFAULT_MAX_NOTES)
      end

      def positive_int(value, default)
        parsed = value.to_i
        parsed.positive? ? parsed : default
      end
    end

    def initialize(credential, api_client: CentaurApiClient.new, mcp_http: nil)
      @credential = credential
      @api_client = api_client
      @mcp_http = mcp_http || self.class.mcp_http
      @run_id = "granola_#{SecureRandom.hex(16)}"
      @rpc_id = 0
      @source_user_email = credential.provider_email.to_s.strip.downcase
    end

    def call
      account = parse_account(mcp_tool("get_account_info"))
      @source_user_email = account["email"].to_s.strip.downcase.presence || @source_user_email
      raise GranolaApiError, "Granola account did not provide an email" if @source_user_email.blank?

      checkpoint = load_checkpoint
      notes = sync_notes(checkpoint)
      @api_client.ingest_granola_sync_batch(success_batch(notes, checkpoint))
    rescue StandardError => error
      record_failure(error)
      raise
    end

    private

    def scope_id
      "oauth:#{@credential.oid}"
    end

    def load_checkpoint
      @api_client.get_granola_sync_checkpoint(scope_id: scope_id).fetch("checkpoint")
    end

    def sync_notes(checkpoint)
      meetings = parse_meetings(
        mcp_tool(
          "list_meetings",
          "time_range" => "custom",
          "custom_start" => range_start(checkpoint),
          "custom_end" => Time.current.utc.to_date.iso8601
        )
      ).first(self.class.max_notes)

      details = parse_meetings(
        mcp_tool("get_meetings", "meeting_ids" => meetings.map { |meeting| meeting.fetch("id") })
      ).index_by { |meeting| meeting.fetch("id") }

      meetings.filter_map do |meeting|
        detailed = details.fetch(meeting.fetch("id"), meeting)
        transcript_text = meeting_transcript(detailed.fetch("id"))
        normalize_note(detailed, transcript_text)
      end
    end

    def meeting_transcript(meeting_id)
      mcp_tool("get_meeting_transcript", "meeting_id" => meeting_id)
    rescue GranolaApiError => error
      # Transcripts are only available on paid Granola plans. Keep syncing the
      # note metadata when that optional tool is unavailable or access is
      # denied, rather than dropping the entire user's sync.
      Rails.logger.info do
        "Granola transcript unavailable for #{meeting_id} on credential #{@credential.oid}: " \
          "#{error.message}"
      end
      ""
    end

    def success_batch(notes, checkpoint)
      transcript_count = notes.count { |note| note["transcript"].present? }
      watermark_time = notes.filter_map { |note| parse_time(note["source_updated_at"]) }.max
      watermark_time ||= parse_time(checkpoint&.fetch("watermark_time", nil))
      watermark_time ||= Time.current.utc

      {
        run: {
          run_id: @run_id,
          mode: "incremental",
          status: "completed",
          scope_id: scope_id,
          broker_credential_id: @credential.oid,
          source_user_email: @source_user_email,
          notes_seen: notes.length,
          notes_upserted: notes.length,
          transcripts_seen: notes.length,
          transcripts_upserted: transcript_count,
          metadata: run_metadata
        },
        notes: notes,
        checkpoint: {
          scope_id: scope_id,
          watermark_time: watermark_time.iso8601
        }
      }
    end

    def record_failure(error)
      return if @source_user_email.blank?

      @api_client.ingest_granola_sync_batch(
        run: {
          run_id: @run_id,
          mode: "incremental",
          status: "failed",
          scope_id: scope_id,
          broker_credential_id: @credential.oid,
          source_user_email: @source_user_email,
          error_text: "#{error.class}: #{error.message}".truncate(2_000),
          metadata: run_metadata
        },
        notes: [],
        checkpoint: { scope_id: scope_id }
      )
    rescue StandardError => report_error
      Rails.logger.warn do
        "Granola sync failure could not be recorded for credential #{@credential.oid}: " \
          "#{report_error.class}: #{report_error.message}"
      end
    end

    def run_metadata
      {
        "oauth_app_slug" => @credential.oauth_app&.slug,
        "credential_id" => @credential.oid,
        "provider_subject" => @credential.provider_subject.to_s
      }
    end

    def range_start(checkpoint)
      watermark = parse_time(checkpoint&.fetch("watermark_time", nil))
      start_time = watermark ? watermark - WATERMARK_OVERLAP_SECONDS : self.class.initial_lookback_days.days.ago
      start_time.utc.to_date.iso8601
    end

    def parse_account(text)
      JSON.parse(text)
    rescue JSON::ParserError
      raise GranolaApiError, "Granola MCP returned an invalid account response"
    end

    def normalize_note(meeting, transcript_text)
      transcript = transcript_text.to_s.strip
      date = parse_mcp_date(meeting["date"])
      {
        "note_id" => meeting.fetch("id"),
        "title" => meeting["title"].to_s,
        "owner" => meeting.fetch("owner", {}),
        "attendees" => meeting.fetch("attendees", []),
        "calendar_event" => {},
        "summary_markdown" => meeting["summary_markdown"].to_s,
        "summary_text" => meeting["summary_markdown"].to_s,
        "transcript" => transcript.present? ? [ { "speaker" => { "source" => "transcript" }, "text" => transcript } ] : [],
        "url" => "",
        "source_created_at" => date,
        "source_updated_at" => date,
        "raw_payload" => {
          "meeting" => meeting,
          "transcript_available" => transcript.present?,
          "source" => "granola_mcp"
        }
      }
    end

    def parse_meetings(text)
      text.to_s.scan(MEETING_RE).filter_map do |id, title, date, body|
        participants = participant_list(body)
        owner = participants.find { |participant| participant["name"].include?("(note creator)") } || participants.first || {}
        owner = owner.merge(
          "name" => owner.fetch("name", "").sub("(note creator)", "").split(" from ", 2).first.strip
        ) unless owner.empty?
        summary_match = body.match(SUMMARY_RE)
        {
          "id" => CGI.unescapeHTML(id),
          "title" => CGI.unescapeHTML(title),
          "date" => CGI.unescapeHTML(date),
          "owner" => owner,
          "attendees" => participants,
          "summary_markdown" => CGI.unescapeHTML(summary_match&.[](:summary).to_s.strip)
        }
      end
    end

    def participant_list(body)
      participants = CGI.unescapeHTML(body.match(PARTICIPANTS_RE)&.[](:participants).to_s)
      participants.scan(PARTICIPANT_RE).map do |name, email|
        { "name" => CGI.unescapeHTML(name).strip, "email" => email.strip.downcase }
      end
    end

    def parse_mcp_date(value)
      match = value.to_s.match(MCP_DATE_RE)
      return nil unless match

      offset = format("%+03d00", (match[:offset].presence || "+0").to_i)
      Time.strptime("#{match[:date]} #{offset}", "%b %d, %Y %I:%M %p %z").iso8601
    rescue ArgumentError
      nil
    end

    def parse_time(value)
      return if value.blank?

      Time.iso8601(value)
    rescue ArgumentError
      nil
    end

    def mcp_tool(name, arguments = {})
      if @mcp_http
        return @mcp_http.call(tool: name, arguments: arguments, access_token: @credential.access_token).to_s
      end

      initialize_mcp_session unless @mcp_initialized
      response = mcp_request(
        "tools/call",
        { name: name, arguments: arguments },
        session_id: @mcp_session_id
      )
      payload = decode_mcp_response(response)
      raise GranolaApiError, "Granola MCP returned #{payload['error']}" if payload["error"]

      result = payload.fetch("result", {})
      raise GranolaApiError, "Granola MCP tool #{name} failed" if result["isError"]

      Array(result["content"])
        .filter_map { |content| content["text"] if content["type"] == "text" }
        .join("\n")
    end

    def initialize_mcp_session
      response = mcp_request(
        "initialize",
        {
          protocolVersion: "2025-03-26",
          capabilities: {},
          clientInfo: { name: "centaur-console", version: "1.0" }
        }
      )
      payload = decode_mcp_response(response)
      raise GranolaApiError, "Granola MCP returned #{payload['error']}" if payload["error"]
      raise GranolaApiError, "Granola MCP did not acknowledge initialization" unless payload["result"]

      @mcp_session_id = response["mcp-session-id"].presence
      send_mcp_initialized_notification if @mcp_session_id
      @mcp_initialized = true
    end

    def send_mcp_initialized_notification
      mcp_request("notifications/initialized", {}, session_id: @mcp_session_id, notification: true)
    rescue GranolaApiError => error
      Rails.logger.warn do
        "Granola MCP initialization notification failed for credential #{@credential.oid}: " \
          "#{error.message}"
      end
    end

    def mcp_request(method, params, session_id: nil, notification: false)
      @rpc_id += 1 unless notification
      uri = URI.parse(MCP_URL)
      request = Net::HTTP::Post.new(uri)
      request["Authorization"] = "Bearer #{@credential.access_token}"
      request["Content-Type"] = "application/json"
      request["Accept"] = "application/json, text/event-stream"
      request["MCP-Protocol-Version"] = "2025-03-26" if session_id.present?
      request["MCP-Session-Id"] = session_id if session_id.present?
      payload = { jsonrpc: "2.0", method: method, params: params }
      payload[:id] = @rpc_id unless notification
      request.body = payload.to_json

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = true
      http.open_timeout = 30
      http.read_timeout = 60
      response = http.request(request)
      unless response.code.to_i.between?(200, 299)
        raise GranolaApiError, "Granola MCP returned HTTP #{response.code}"
      end

      response
    end

    def decode_mcp_response(response)
      body = response.body.to_s
      if response["content-type"].to_s.start_with?("text/event-stream")
        payload = body.each_line.filter_map { |line| line.delete_prefix("data: ").strip if line.start_with?("data: ") }.last
        raise GranolaApiError, "Granola MCP returned an empty event stream" if payload.blank?

        body = payload
      end
      JSON.parse(body)
    rescue JSON::ParserError
      raise GranolaApiError, "Granola MCP returned malformed JSON"
    end
  end
end
