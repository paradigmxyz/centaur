require "test_helper"

module SlackDm
  class SyncCredentialTest < ActiveSupport::TestCase
    class FakeApiClient
      attr_reader :batch

      def list_slack_dm_sync_checkpoints(broker_credential_id:, home_team_id:)
        {
          "checkpoints" => [
            {
              "broker_credential_id" => broker_credential_id,
              "home_team_id" => home_team_id,
              "conversation_id" => "D123",
              "watermark_ts" => "1700000000.000001"
            }
          ]
        }
      end

      def ingest_slack_dm_sync_batch(payload)
        @batch = payload
        { "ok" => true }
      end
    end

    def slack_app
      OauthApp.create!(
        provider: "slack",
        slug: "slack-dms-#{SecureRandom.hex(6)}",
        client_id: "slack-client-#{SecureRandom.hex(4)}",
        client_secret: "secret",
        allowed_scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
        credential_namespace: "acme",
        created_by: users(:acme_admin)
      )
    end

    def credential
      @credential ||= BrokerCredential.create!(
        oauth_app: slack_app,
        namespace: "acme",
        foreign_id: "slack-dms-#{SecureRandom.hex(6)}",
        token_endpoint: "https://slack.com/api/oauth.v2.access",
        access_token: "xoxp-live",
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
        provider_subject: "U_ME"
      )
    end

    test "sync normalizes conversations members messages files and checkpoints" do
      api_client = FakeApiClient.new
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          {
            "ok" => true,
            "channels" => [
              {
                "id" => "D123",
                "is_im" => true,
                "is_mpim" => false,
                "user" => "U_OTHER",
                "is_archived" => false
              }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_MEMBERS_ENDPOINT
          {
            "ok" => true,
            "members" => [ "U_OTHER", "U_ME" ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_HISTORY_ENDPOINT
          assert_equal "D123", params["channel"]
          assert_equal "1700000000.000001", params["oldest"]
          {
            "ok" => true,
            "messages" => [
              {
                "type" => "message",
                "ts" => "1700000000.000002",
                "thread_ts" => "1700000000.000002",
                "user" => "U_OTHER",
                "text" => "hello",
                "reply_count" => 1,
                "reply_users" => [ "U_ME" ],
                "latest_reply" => "1700000000.000003",
                "files" => [
                  {
                    "id" => "F123",
                    "name" => "note.txt",
                    "title" => "Note",
                    "mimetype" => "text/plain",
                    "filetype" => "text",
                    "size" => 42,
                    "url_private" => "https://files.example/private",
                    "permalink" => "https://slack.example/file"
                  }
                ]
              }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_REPLIES_ENDPOINT
          assert_equal "1700000000.000002", params["ts"]
          {
            "ok" => true,
            "messages" => [
              {
                "type" => "message",
                "ts" => "1700000000.000002",
                "user" => "U_OTHER",
                "text" => "hello"
              },
              {
                "type" => "message",
                "ts" => "1700000000.000003",
                "user" => "U_ME",
                "text" => "reply"
              }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        else
          flunk "unexpected Slack endpoint #{endpoint}"
        end
      end

      SlackDm::SyncCredential.new(
        credential,
        api_client: api_client,
        slack_api_http: slack_http
      ).call

      batch = api_client.batch
      assert_equal "completed", batch[:run][:status]
      assert_equal credential.oid, batch[:run][:broker_credential_id]
      assert_equal 1, batch[:conversations].length
      assert_equal "im", batch[:conversations].first[:conversation_type]
      assert_equal %w[U_OTHER U_ME], batch[:members].map { |member| member[:user_id] }
      assert_equal 2, batch[:messages].length
      assert_equal "hello", batch[:messages].first[:text]
      assert_equal "1700000000.000002", batch[:messages].last[:parent_message_ts]
      assert_equal "F123", batch[:attachments].first[:slack_file_id]
      assert_equal "1700000000.000002", batch[:checkpoints].first[:watermark_ts]
    end
  end
end
