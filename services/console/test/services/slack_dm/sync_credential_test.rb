require "test_helper"

module SlackDm
  class SyncCredentialTest < ActiveSupport::TestCase
    class FakeApiClient
      attr_reader :batches

      def initialize(&ingest_handler)
        @batches = []
        @ingest_handler = ingest_handler
      end

      def batch
        @batches.find { |payload| payload[:conversations].any? }
      end

      def final_batch
        @batches.last
      end

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
        @batches << payload
        @ingest_handler&.call(payload)
        { "ok" => true }
      end
    end

    class FakeHttpClient
      def initialize(response)
        @response = response
      end

      def get(*)
        @response
      end
    end

    def slack_app
      OauthApp.create!(
        provider: "slack",
        slug: "slack-dms-#{SecureRandom.hex(6)}",
        client_id: "slack-client-#{SecureRandom.hex(4)}",
        client_secret: "secret",
        allowed_scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
        created_by: users(:acme_admin)
      )
    end

    def credential
      @credential ||= BrokerCredential.create!(
        oauth_app: slack_app,
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

    def two_conversation_slack_http(history_calls)
      lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          {
            "ok" => true,
            "channels" => [
              { "id" => "D100", "is_im" => true, "user" => "U_ONE" },
              { "id" => "D200", "is_im" => true, "user" => "U_TWO" }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_HISTORY_ENDPOINT
          conversation_id = params.fetch("channel")
          history_calls << conversation_id
          {
            "ok" => true,
            "messages" => [
              {
                "type" => "message",
                "ts" => conversation_id == "D100" ? "1700000000.000001" : "1700000000.000002",
                "user" => "U_OTHER",
                "text" => conversation_id
              }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        else
          flunk "unexpected Slack endpoint #{endpoint}"
        end
      end
    end

    test "uses an extended API read timeout by default" do
      api_client = Object.new
      client_created = false
      factory = lambda do |read_timeout:|
        assert_equal SlackDm::SyncCredential::API_READ_TIMEOUT_SECONDS, read_timeout
        client_created = true
        api_client
      end

      CentaurApiClient.stub(:new, factory) do
        SlackDm::SyncCredential.new(Object.new)
      end

      assert client_created
    end

    test "oauth_app_slug defaults to slack and honors console env prefix" do
      env_key = "CENTAUR_CONSOLE_SLACK_DM_SYNC_OAUTH_APP_SLUG"
      legacy_env_key = "IRON_CONTROL_SLACK_DM_SYNC_OAUTH_APP_SLUG"
      previous = {
        env_key => ENV[env_key],
        legacy_env_key => ENV[legacy_env_key]
      }
      ENV.delete(env_key)
      ENV.delete(legacy_env_key)

      assert_equal "slack", SlackDm::SyncCredential.oauth_app_slug

      ENV[env_key] = "custom-slack"
      assert_equal "custom-slack", SlackDm::SyncCredential.oauth_app_slug
    ensure
      previous.each do |key, value|
        if value.nil?
          ENV.delete(key)
        else
          ENV[key] = value
        end
      end
    end

    test "sync normalizes conversations members messages files and checkpoints" do
      api_client = FakeApiClient.new
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          assert_equal "im,mpim,private_channel", params["types"]
          {
            "ok" => true,
            "channels" => [
              {
                "id" => "D123",
                "is_im" => true,
                "is_mpim" => false,
                "user" => "U_OTHER",
                "is_archived" => false,
                "purpose" => { "value" => "direct\u0000message" }
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
                "text" => "hel\u0000lo",
                "blocks" => [ { "text" => { "text" => "hel\u0000lo" } } ],
                "reply_count" => 1,
                "reply_users" => [ "U_ME" ],
                "latest_reply" => "1700000000.000003",
                "files" => [
                  {
                    "id" => "F123",
                    "name" => "no\u0000te.txt",
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
      assert_equal "running", batch[:run][:status]
      assert_equal credential.oid, batch[:run][:broker_credential_id]
      assert_equal 1, batch[:conversations].length
      assert_equal "im", batch[:conversations].first[:conversation_type]
      assert_equal %w[U_OTHER U_ME], batch[:members].map { |member| member[:user_id] }
      assert_equal 2, batch[:messages].length
      assert_equal "hello", batch[:messages].first[:text]
      assert_equal "hello", batch[:messages].first[:raw_payload].dig("blocks", 0, "text", "text")
      assert_equal "directmessage", batch[:conversations].first[:raw_payload].dig("purpose", "value")
      assert_equal "1700000000.000002", batch[:messages].last[:parent_message_ts]
      assert_equal "F123", batch[:attachments].first[:slack_file_id]
      assert_equal "note.txt", batch[:attachments].first[:name]
      assert_equal "note.txt", batch[:attachments].first[:raw_payload]["name"]
      assert_equal "1700000000.000002", batch[:checkpoints].first[:watermark_ts]
      assert_equal "completed", api_client.final_batch[:run][:status]
      assert api_client.final_batch[:run][:finished]
    end

    test "sync ingests private channels and their complete member list" do
      api_client = FakeApiClient.new
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          assert_equal "im,mpim,private_channel", params["types"]
          {
            "ok" => true,
            "channels" => [
              {
                "id" => "G123",
                "name" => "leadership",
                "is_private" => true,
                "is_archived" => false
              }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_MEMBERS_ENDPOINT
          assert_equal "G123", params["channel"]
          {
            "ok" => true,
            "members" => %w[U_ME U_OTHER],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_HISTORY_ENDPOINT
          {
            "ok" => true,
            "messages" => [
              {
                "type" => "message",
                "ts" => "1700000001.000001",
                "user" => "U_OTHER",
                "text" => "private roadmap"
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
      assert_equal "private_channel", batch[:conversations].first[:conversation_type]
      assert_equal "leadership", batch[:conversations].first[:raw_payload]["name"]
      assert_equal %w[U_ME U_OTHER], batch[:members].map { |member| member[:user_id] }
      assert_equal "private roadmap", batch[:messages].first[:text]
    end

    test "sync never replaces membership from truncated pagination" do
      env_key = "CENTAUR_CONSOLE_SLACK_DM_SYNC_MEMBERS_MAX_PAGES"
      previous = ENV[env_key]
      ENV[env_key] = "1"
      api_client = FakeApiClient.new
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          {
            "ok" => true,
            "channels" => [ { "id" => "G123", "is_private" => true } ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_MEMBERS_ENDPOINT
          {
            "ok" => true,
            "members" => [ "U_ME" ],
            "response_metadata" => { "next_cursor" => "more" }
          }
        else
          flunk "unexpected Slack endpoint #{endpoint} with #{params}"
        end
      end

      error = assert_raises(SlackApi::Error) do
        SlackDm::SyncCredential.new(
          credential,
          api_client: api_client,
          slack_api_http: slack_http
        ).call
      end
      assert_match "membership pagination truncated", error.message
      assert_nil api_client.batch
    ensure
      previous.nil? ? ENV.delete(env_key) : ENV[env_key] = previous
    end

    test "sync ingests completed conversations separately and resumes at its cursor" do
      api_client = FakeApiClient.new
      rate_limited = true
      history_calls = []
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          {
            "ok" => true,
            "channels" => [
              { "id" => "D100", "is_im" => true, "user" => "U_ONE" },
              { "id" => "D200", "is_im" => true, "user" => "U_TWO" }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_HISTORY_ENDPOINT
          conversation_id = params.fetch("channel")
          history_calls << conversation_id
          if conversation_id == "D200" && rate_limited
            raise SlackApi::RateLimitedError.new(retry_after: 20.minutes.to_i)
          end

          {
            "ok" => true,
            "messages" => [
              {
                "type" => "message",
                "ts" => "1700000000.000001",
                "user" => "U_OTHER",
                "text" => conversation_id
              }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        else
          flunk "unexpected Slack endpoint #{endpoint}"
        end
      end
      conversation_cursor = nil

      assert_raises(SlackApi::RateLimitedError) do
        SlackDm::SyncCredential.new(
          credential,
          api_client: api_client,
          slack_api_http: slack_http
        ).call do |conversation_id|
          conversation_cursor = conversation_id
        end
      end

      assert_equal "D200", conversation_cursor
      ingested_conversation_ids = api_client.batches.flat_map do |batch|
        batch[:conversations].map { |conversation| conversation[:conversation_id] }
      end
      assert_equal [ "D100" ], ingested_conversation_ids

      rate_limited = false
      SlackDm::SyncCredential.new(
        credential,
        api_client: api_client,
        slack_api_http: slack_http
      ).call(starting_conversation_id: conversation_cursor)

      assert_equal %w[D100 D200 D200], history_calls
      ingested_conversation_ids = api_client.batches.flat_map do |batch|
        batch[:conversations].map { |conversation| conversation[:conversation_id] }
      end
      assert_equal %w[D100 D200], ingested_conversation_ids
    end

    test "sync checkpoints the next conversation before stopping at its deadline" do
      api_client = FakeApiClient.new
      conversation_cursor = nil
      now = Time.zone.parse("2026-08-23 12:00:00")
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          {
            "ok" => true,
            "channels" => [
              { "id" => "D100", "is_im" => true, "user" => "U_ONE" },
              { "id" => "D200", "is_im" => true, "user" => "U_TWO" }
            ],
            "response_metadata" => { "next_cursor" => "" }
          }
        when SlackDm::SyncCredential::CONVERSATIONS_HISTORY_ENDPOINT
          travel 31.minutes if params.fetch("channel") == "D100"
          { "ok" => true, "messages" => [], "response_metadata" => { "next_cursor" => "" } }
        else
          flunk "unexpected Slack endpoint #{endpoint}"
        end
      end

      completed = travel_to(now) do
        SlackDm::SyncCredential.new(
          credential,
          api_client: api_client,
          slack_api_http: slack_http
        ).call(deadline: now + 30.minutes) do |conversation_id|
          conversation_cursor = conversation_id
        end
      end

      assert_not completed
      assert_equal "D200", conversation_cursor
      ingested_conversation_ids = api_client.batches.flat_map do |batch|
        batch[:conversations].map { |conversation| conversation[:conversation_id] }
      end
      assert_equal [ "D100" ], ingested_conversation_ids
      assert_equal "partial", api_client.final_batch[:run][:status]
    end

    test "sync skips a conversation rejected by the ingest API" do
      api_client = FakeApiClient.new do |payload|
        conversation_id = payload.dig(:conversations, 0, :conversation_id)
        if conversation_id == "D100"
          raise CentaurApiClient::Error.new("invalid message timestamp", status: 400)
        end
      end
      history_calls = []
      conversation_cursor = nil

      completed = SlackDm::SyncCredential.new(
        credential,
        api_client: api_client,
        slack_api_http: two_conversation_slack_http(history_calls)
      ).call do |conversation_id|
        conversation_cursor = conversation_id
      end

      attempted_conversation_ids = api_client.batches.filter_map do |batch|
        batch.dig(:conversations, 0, :conversation_id)
      end
      assert completed
      assert_equal %w[D100 D200], history_calls
      assert_equal %w[D100 D200], attempted_conversation_ids
      assert_equal "D200", conversation_cursor
      assert_equal "partial", api_client.final_batch[:run][:status]
      assert_equal 1, api_client.final_batch[:run][:conversations_failed]
      assert_equal 1, api_client.final_batch[:run][:conversations_synced]
      assert_equal 2, api_client.final_batch[:run][:messages_fetched]
      assert_equal 1, api_client.final_batch[:run][:messages_upserted]
    end

    test "sync retries a conversation after a transient ingest API failure" do
      api_client = FakeApiClient.new do |payload|
        conversation_id = payload.dig(:conversations, 0, :conversation_id)
        if conversation_id == "D100"
          raise CentaurApiClient::Error.new("temporary failure", status: 500)
        end
      end
      history_calls = []
      conversation_cursor = nil

      error = assert_raises(CentaurApiClient::Error) do
        SlackDm::SyncCredential.new(
          credential,
          api_client: api_client,
          slack_api_http: two_conversation_slack_http(history_calls)
        ).call do |conversation_id|
          conversation_cursor = conversation_id
        end
      end

      attempted_conversation_ids = api_client.batches.filter_map do |batch|
        batch.dig(:conversations, 0, :conversation_id)
      end
      assert_equal 500, error.status
      assert_equal [ "D100" ], history_calls
      assert_equal [ "D100" ], attempted_conversation_ids
      assert_equal "D100", conversation_cursor
    end

    test "short rate limits retry the same paginated Slack call" do
      api_client = FakeApiClient.new
      list_cursors = []
      rate_limited = true
      slack_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "xoxp-live", access_token
        case endpoint
        when SlackDm::SyncCredential::AUTH_TEST_ENDPOINT
          { "ok" => true, "team_id" => "T123", "user_id" => "U_ME" }
        when SlackDm::SyncCredential::CONVERSATIONS_LIST_ENDPOINT
          cursor = params["cursor"]
          list_cursors << cursor
          if cursor.nil?
            {
              "ok" => true,
              "channels" => [],
              "response_metadata" => { "next_cursor" => "page-2" }
            }
          elsif rate_limited
            rate_limited = false
            raise SlackApi::RateLimitedError.new(retry_after: 5.minutes.to_i - 1)
          else
            {
              "ok" => true,
              "channels" => [],
              "response_metadata" => { "next_cursor" => "" }
            }
          end
        else
          flunk "unexpected Slack endpoint #{endpoint}"
        end
      end
      client = SlackDm::SyncCredential.new(
        credential,
        api_client: api_client,
        slack_api_http: slack_http
      )
      sleeps = []

      client.stub(:sleep, ->(seconds) { sleeps << seconds }) do
        assert client.call
      end

      assert_equal [ 5.minutes.to_i - 1 ], sleeps
      assert_equal [ nil, "page-2", "page-2" ], list_cursors
    end

    test "short rate limits escape after five retries of one Slack call" do
      attempts = 0
      slack_http = lambda do |endpoint:, **|
        assert_equal SlackDm::SyncCredential::AUTH_TEST_ENDPOINT, endpoint
        attempts += 1
        raise SlackApi::RateLimitedError.new(retry_after: 1)
      end
      client = SlackDm::SyncCredential.new(credential, slack_api_http: slack_http)
      sleeps = []

      error = assert_raises(SlackApi::RateLimitedError) do
        client.stub(:sleep, ->(seconds) { sleeps << seconds }) { client.call }
      end

      assert_equal 1, error.retry_after
      assert_equal SlackDm::SyncCredential::MAX_INLINE_RATE_LIMIT_RETRIES + 1, attempts
      assert_equal Array.new(SlackDm::SyncCredential::MAX_INLINE_RATE_LIMIT_RETRIES, 1), sleeps
    end

    test "long 429 responses expose the full Retry-After to the cursor job" do
      [
        [ "300", 300 ],
        [ "600", 600 ]
      ].each do |header, expected|
        response = HttpClient::Response.new(
          status: 429,
          body: "",
          headers: { "retry-after" => header }
        )

        error = assert_raises(SlackApi::RateLimitedError) do
          SlackDm::SyncCredential.new(
            credential,
            http_client: FakeHttpClient.new(response)
          ).call
        end

        assert_equal expected, error.retry_after
      end
    end

    test "transient Slack API errors request deferred job execution" do
      %w[fatal_error internal_error].each do |error_code|
        response = HttpClient::Response.new(
          status: 200,
          body: { ok: false, error: error_code }.to_json,
          headers: { "content-type" => "application/json" }
        )

        error = assert_raises(SlackApi::TransientError) do
          SlackDm::SyncCredential.new(
            credential,
            http_client: FakeHttpClient.new(response)
          ).call
        end

        assert_equal SlackApi::DEFAULT_TRANSIENT_RETRY_AFTER_SECONDS, error.retry_after
      end
    end

    test "hostname resolution failures request deferred job execution" do
      http_client = Object.new
      http_client.define_singleton_method(:get) do |*|
        raise Socket::ResolutionError, "Temporary failure in name resolution"
      end

      error = assert_raises(SlackApi::TransientError) do
        SlackDm::SyncCredential.new(credential, http_client: http_client).call
      end

      assert_equal "hostname_resolution_failed", error.code
      assert_equal SlackApi::DEFAULT_TRANSIENT_RETRY_AFTER_SECONDS, error.retry_after
      assert_instance_of Socket::ResolutionError, error.cause
    end
  end
end
