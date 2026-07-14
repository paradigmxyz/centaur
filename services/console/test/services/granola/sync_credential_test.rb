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
        credential_namespace: "acme",
        created_by: users(:acme_admin)
      )
    end

    def credential
      @credential ||= BrokerCredential.create!(
        oauth_app: granola_app,
        namespace: "acme",
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

    private

    def meeting_xml
      <<~XML
        <meeting id="meeting-1" title="Planning" date="Jul 8, 2026 5:30 PM GMT+2">
          <known_participants>Ada (note creator) from Acme &lt;ada@example.com&gt;
          Bob &lt;bob@example.com&gt;</known_participants>
          <summary>Ship the Granola sync.</summary>
        </meeting>
      XML
    end
  end
end
