require "test_helper"

module Granola
  class SyncCredentialTest < ActiveSupport::TestCase
    class FakeApiClient
      attr_reader :batches

      def initialize(checkpoint: nil)
        @checkpoint = checkpoint
        @batches = []
      end

      def get_granola_sync_checkpoint(scope_id:)
        { "ok" => true, "checkpoint" => @checkpoint&.merge("scope_id" => scope_id) }
      end

      def ingest_granola_sync_batch(payload)
        @batches << payload
        { "ok" => true }
      end
    end

    def granola_app
      @granola_app ||= OauthApp.create!(
        provider: "granola",
        slug: "granola-sync-#{SecureRandom.hex(6)}",
        client_id: "granola-client",
        client_secret: "granola-secret",
        allowed_scopes: %w[meetings:read],
        created_by: users(:acme_admin)
      )
    end

    def credential
      @credential ||= BrokerCredential.create!(
        oauth_app: granola_app,
        foreign_id: "granola-sync-#{SecureRandom.hex(6)}",
        token_endpoint: Oauth::Providers::Granola::TOKEN_ENDPOINT,
        access_token: "granola-access-token",
        refresh_token: "granola-refresh-token",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: %w[meetings:read],
        provider_subject: "granola-user-1",
        provider_email: "owner@example.com"
      )
    end

    test "syncs the connected user's notes into a credential-scoped batch" do
      api_client = FakeApiClient.new(
        checkpoint: { "watermark_time" => "2026-07-08T12:00:00Z" }
      )
      mcp_http = lambda do |tool:, arguments:, access_token:|
        assert_equal "granola-access-token", access_token

        case tool
        when "get_account_info"
          { email: "Owner@Example.com", workspace: "Acme" }.to_json
        when "list_meetings"
          assert_equal "custom", arguments.fetch("time_range")
          assert arguments.fetch("custom_start") <= arguments.fetch("custom_end")
          meeting_xml
        when "get_meetings"
          assert_equal [ "meeting-1" ], arguments.fetch("meeting_ids")
          meeting_xml
        when "get_meeting_transcript"
          assert_equal "meeting-1", arguments.fetch("meeting_id")
          "Ada: Ship the Granola sync."
        else
          flunk "unexpected Granola MCP tool #{tool}"
        end
      end

      SyncCredential.new(credential, api_client: api_client, mcp_http: mcp_http).call

      batch = api_client.batches.fetch(0)
      assert_equal "completed", batch[:run][:status]
      assert_equal "oauth:#{credential.oid}", batch[:run][:scope_id]
      assert_equal credential.oid, batch[:run][:broker_credential_id]
      assert_equal "owner@example.com", batch[:run][:source_user_email]
      assert_equal "oauth:#{credential.oid}", batch[:checkpoint][:scope_id]

      note = batch[:notes].fetch(0)
      assert_equal "meeting-1", note["note_id"]
      assert_equal "Planning", note["title"]
      assert_equal "ada@example.com", note["owner"]["email"]
      assert_equal "Ada", note["owner"]["name"]
      assert_equal [ "ada@example.com", "bob@example.com" ], note["attendees"].pluck("email")
      assert_equal "2026-07-08T17:30:00+02:00", note["source_updated_at"]
      assert_equal "Ada: Ship the Granola sync.", note["transcript"].first["text"]
    end

    test "records an API failure against the same OAuth credential scope" do
      api_client = FakeApiClient.new
      mcp_http = lambda do |tool:, **|
        case tool
        when "get_account_info"
          { email: "owner@example.com" }.to_json
        when "list_meetings"
          raise SyncCredential::GranolaApiError, "rate limited"
        else
          flunk "unexpected Granola MCP tool #{tool}"
        end
      end

      assert_raises(SyncCredential::GranolaApiError) do
        SyncCredential.new(credential, api_client: api_client, mcp_http: mcp_http).call
      end

      failed = api_client.batches.fetch(0)
      assert_equal "failed", failed[:run][:status]
      assert_equal "oauth:#{credential.oid}", failed[:run][:scope_id]
      assert_includes failed[:run][:error_text], "rate limited"
    end

    test "parses meeting dates with timezone abbreviations" do
      api_client = FakeApiClient.new
      cst_meeting = meeting_xml(date: "Aug 5, 2026 5:00 PM CST")
      mcp_http = lambda do |tool:, **|
        case tool
        when "get_account_info"
          { email: "owner@example.com" }.to_json
        when "list_meetings", "get_meetings"
          cst_meeting
        when "get_meeting_transcript"
          ""
        else
          flunk "unexpected Granola MCP tool #{tool}"
        end
      end

      SyncCredential.new(credential, api_client: api_client, mcp_http: mcp_http).call

      batch = api_client.batches.fetch(0)
      assert_equal "2026-08-05T17:00:00-06:00", batch[:notes].fetch(0)["source_created_at"]
      assert_equal "2026-08-05T17:00:00-06:00", batch[:notes].fetch(0)["source_updated_at"]
      assert_equal "2026-08-05T17:00:00-06:00", batch[:checkpoint][:watermark_time]
    end

    test "records a successful sync when there are no new meetings" do
      checkpoint_time = "2026-07-08T12:00:00Z"
      api_client = FakeApiClient.new(
        checkpoint: { "watermark_time" => checkpoint_time }
      )
      mcp_http = lambda do |tool:, **|
        case tool
        when "get_account_info"
          { email: "owner@example.com" }.to_json
        when "list_meetings"
          ""
        else
          flunk "unexpected Granola MCP tool #{tool}"
        end
      end

      SyncCredential.new(credential, api_client: api_client, mcp_http: mcp_http).call

      batch = api_client.batches.fetch(0)
      assert_equal "completed", batch[:run][:status]
      assert_equal 0, batch[:run][:notes_seen]
      assert_empty batch[:notes]
      assert_equal checkpoint_time, batch[:checkpoint][:watermark_time]
    end

    test "does not advance the checkpoint when reported meetings cannot be parsed" do
      api_client = FakeApiClient.new
      mcp_http = lambda do |tool:, **|
        case tool
        when "get_account_info"
          { email: "owner@example.com" }.to_json
        when "list_meetings"
          '<meetings_data count="1"><meeting></meeting></meetings_data>'
        else
          flunk "unexpected Granola MCP tool #{tool}"
        end
      end

      error = assert_raises(SyncCredential::GranolaApiError) do
        SyncCredential.new(credential, api_client: api_client, mcp_http: mcp_http).call
      end

      assert_equal "Granola MCP reported meetings that could not be parsed", error.message
      assert_nil api_client.batches.fetch(0)[:checkpoint][:watermark_time]
    end

    test "fetches all meeting details in batches of ten" do
      api_client = FakeApiClient.new
      meetings = 51.times.map { |index| meeting_xml(id: "meeting-#{index}") }
      detail_batches = []
      mcp_http = lambda do |tool:, arguments: {}, **|
        case tool
        when "get_account_info"
          { email: "owner@example.com" }.to_json
        when "list_meetings"
          meetings.join
        when "get_meetings"
          detail_batches << arguments.fetch("meeting_ids")
          indexes = arguments.fetch("meeting_ids").map { |id| id.delete_prefix("meeting-").to_i }
          meetings.values_at(*indexes).join
        when "get_meeting_transcript"
          ""
        else
          flunk "unexpected Granola MCP tool #{tool}"
        end
      end

      SyncCredential.new(credential, api_client: api_client, mcp_http: mcp_http).call

      assert_equal [ 10, 10, 10, 10, 10, 1 ], detail_batches.map(&:length)
      assert_equal 51, api_client.batches.fetch(0)[:notes].length
    end

    test "includes MCP tool error content in the raised error" do
      sync = SyncCredential.new(credential, api_client: FakeApiClient.new)
      sync.instance_variable_set(:@mcp_initialized, true)

      response = HttpClient::Response.new(
        status: 200,
        headers: { "content-type" => "application/json" },
        body: {
          result: {
            isError: true,
            content: [ { type: "text", text: "Invalid meeting_ids: maximum 10" } ]
          }
        }.to_json
      )
      mcp_request = Minitest::Mock.new
      mcp_request.expect(:call, response) do |*_args|
        true
      end

      sync.stub(:mcp_request, ->(*_args) { mcp_request.call }) do
        error = assert_raises(SyncCredential::GranolaApiError) do
          sync.send(:mcp_tool, "get_meetings", "meeting_ids" => %w[meeting-1 meeting-2])
        end

        assert_equal(
          "Granola MCP tool get_meetings failed: Invalid meeting_ids: maximum 10",
          error.message
        )
      end

      mcp_request.verify
    end

    private

    def meeting_xml(id: "meeting-1", date: "Jul 8, 2026 5:30 PM GMT+2")
      <<~XML
        <meeting id="#{id}" title="Planning" date="#{date}" captured_by_me="true" listed_as_participant="true" is_workspace_visible="false">
          <known_participants>Ada (note creator) from Acme &lt;ada@example.com&gt;
          Bob &lt;bob@example.com&gt;</known_participants>
          <summary>Ship the Granola sync.</summary>
        </meeting>
      XML
    end
  end
end
