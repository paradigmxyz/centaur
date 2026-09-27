require "test_helper"

module GoogleDocs
  class JobsTest < ActiveJob::TestCase
    SYNC_ENABLED_ENV = "CENTAUR_CONSOLE_GOOGLE_DOCS_SYNC_ENABLED"

    class FakeApiClient
      attr_accessor :checkpoint, :missing
      attr_reader :batches

      def initialize(checkpoint: nil, missing: nil)
        @checkpoint = checkpoint
        @missing = missing
        @batches = []
      end

      def get_google_docs_sync_checkpoint(broker_credential_id:)
        { "checkpoint" => checkpoint }
      end

      def ingest_google_docs_sync_batch(payload)
        @batches << payload
        update_checkpoint(payload[:checkpoint]) if payload[:checkpoint]
        { "ok" => true }
      end

      def get_google_docs_content_status(files:)
        requested = files.map(&:deep_stringify_keys)
        { "missing" => missing.nil? ? requested : missing }
      end

      private

      def update_checkpoint(payload)
        @checkpoint = (checkpoint || {}).merge(payload.deep_stringify_keys)
      end
    end

    setup do
      @previous_sync_enabled = ENV[SYNC_ENABLED_ENV]
      ENV[SYNC_ENABLED_ENV] = "true"
    end

    teardown do
      if @previous_sync_enabled.nil?
        ENV.delete(SYNC_ENABLED_ENV)
      else
        ENV[SYNC_ENABLED_ENV] = @previous_sync_enabled
      end
    end

    def create_google_app
      OauthApp.create!(
        provider: "google",
        slug: "google-docs-job-#{SecureRandom.hex(6)}",
        client_id: "google-client",
        client_secret: "secret",
        allowed_scopes: [ SyncCredential::DRIVE_READONLY_SCOPE ],
        enabled: true,
        created_by: users(:acme_admin)
      )
    end

    def create_credential(app: create_google_app)
      BrokerCredential.create!(
        oauth_app: app,
        foreign_id: "google-docs-job-#{SecureRandom.hex(6)}",
        token_endpoint: "https://oauth2.googleapis.com/token",
        access_token: "token",
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: [ SyncCredential::DRIVE_READONLY_SCOPE ],
        provider_subject: "google-subject-#{SecureRandom.hex(4)}",
        provider_email: "person@example.com"
      )
    end

    test "poll job chooses initial or incremental sync from the completed user checkpoint" do
      app = create_google_app
      credential = create_credential(app: app)
      api_client = FakeApiClient.new

      CentaurApiClient.stub(:new, api_client) { PollSyncJob.perform_now(app.slug) }
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])

      clear_enqueued_jobs
      api_client.checkpoint = checkpoint_for(credential, user_changes_page_token: "change-200")
      CentaurApiClient.stub(:new, api_client) { PollSyncJob.perform_now(app.slug) }
      assert_enqueued_with(job: IncrementalSyncJob, args: [ credential.id ])
    end

    test "sync kill switch prevents polling and queued ETL work" do
      app = create_google_app
      credential = create_credential(app: app)
      api_client = -> { flunk "disabled Google Docs sync should not create an API client" }
      google_http = ->(**) { flunk "disabled Google Docs sync should not call Google" }

      with_sync_enabled("false") do
        CentaurApiClient.stub(:new, api_client) { PollSyncJob.perform_now(app.slug) }
        with_clients(api_client, google_http) do
          InitialSyncJob.perform_now(credential.id)
          FetchDocumentJob.perform_now(credential.id, google_doc)
        end
      end

      assert_no_enqueued_jobs
    end

    test "initial sync ingests bounded pages before sweeping and publishing its user checkpoint" do
      credential = create_credential
      api_client = FakeApiClient.new
      first_file = google_doc
      second_file = google_doc("doc-456")
      file_page_tokens = []
      google_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "token", access_token
        case endpoint
        when SyncCredential::START_PAGE_TOKEN_ENDPOINT
          { "startPageToken" => "change-100" }
        when SyncCredential::FILES_LIST_ENDPOINT
          file_page_tokens << params["pageToken"]
          if params["pageToken"]
            { "files" => [ second_file ] }
          else
            { "files" => [ first_file ], "nextPageToken" => "files-2" }
          end
        else
          flunk "unexpected Google endpoint #{endpoint}"
        end
      end

      with_clients(api_client, google_http) do
        InitialSyncJob.perform_now(credential.id)
      end

      run_id = api_client.batches.first.dig(:run, :run_id)
      assert_equal [ nil, "files-2" ], file_page_tokens
      assert_equal 3, api_client.batches.length
      first_page = api_client.batches.first
      assert_equal "running", first_page.dig(:run, :status)
      assert_equal [ "doc-123" ], first_page[:files].pluck(:file_id)
      assert_equal [ "doc-123" ], first_page[:observations].pluck(:observed_file_id)
      refute first_page.key?(:observation_sweeps)
      refute first_page.key?(:checkpoint)
      second_page = api_client.batches.second
      assert_equal run_id, second_page.dig(:run, :run_id)
      assert_equal 2, second_page.dig(:run, :files_seen)
      assert_equal [ "doc-456" ], second_page[:files].pluck(:file_id)
      refute second_page.key?(:observation_sweeps)
      refute second_page.key?(:checkpoint)

      completion = api_client.batches.third
      assert_equal "completed", completion.dig(:run, :status)
      assert_equal 2, completion.dig(:run, :files_seen)
      assert_equal(
        [ { broker_credential_id: credential.oid, source_run_id: run_id } ],
        completion[:observation_sweeps]
      )
      checkpoint = completion.fetch(:checkpoint)
      assert_equal "change-100", checkpoint[:changes_page_token]
      assert_equal({}, checkpoint[:metadata])
      assert checkpoint[:last_full_sync_at].present?
      assert_enqueued_with(job: FetchDocumentJob, args: [ credential.id, first_file ])
      assert_enqueued_with(job: FetchDocumentJob, args: [ credential.id, second_file ])
      assert_enqueued_with(job: IncrementalSyncJob, args: [ credential.id ])
    end

    test "a stale initial job with a completed user checkpoint is a no-op" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(credential, user_changes_page_token: "change-200")
      )
      google_http = ->(**) { flunk "a stale initial job should not call Google" }

      with_clients(api_client, google_http) do
        InitialSyncJob.perform_now(credential.id)
      end

      assert_empty api_client.batches
      assert_no_enqueued_jobs
    end

    test "a rejected initial page token leaves no user checkpoint and restarts the crawl" do
      credential = create_credential
      api_client = FakeApiClient.new
      google_http = lambda do |endpoint:, params:, **|
        case endpoint
        when SyncCredential::START_PAGE_TOKEN_ENDPOINT
          { "startPageToken" => "change-100" }
        when SyncCredential::FILES_LIST_ENDPOINT
          if params["pageToken"]
            raise SyncCredential::InvalidPageTokenError, "Page token expired"
          end
          { "files" => [ google_doc ], "nextPageToken" => "files-expired" }
        else
          flunk "unexpected Google endpoint #{endpoint}"
        end
      end

      with_clients(api_client, google_http) { InitialSyncJob.perform_now(credential.id) }

      assert_equal "", api_client.checkpoint["changes_page_token"]
      assert_equal 2, api_client.batches.length
      refute api_client.batches.first.key?(:observation_sweeps)
      refute api_client.batches.first.key?(:checkpoint)
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
    end

    test "incremental sync drains all pages before advancing its user checkpoint" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(credential, user_changes_page_token: "change-100")
      )
      file = google_doc
      change_page_tokens = []
      google_http = lambda do |endpoint:, params:, **|
        assert_equal SyncCredential::CHANGES_LIST_ENDPOINT, endpoint
        change_page_tokens << params["pageToken"]
        if params["pageToken"] == "change-100"
          {
            "changes" => [ { "fileId" => "doc-123", "file" => file } ],
            "nextPageToken" => "change-101"
          }
        else
          {
            "changes" => [
              { "changeType" => "drive", "driveId" => "shared-drive-1" },
              { "fileId" => "doc-removed", "removed" => true }
            ],
            "newStartPageToken" => "change-200"
          }
        end
      end

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_equal [ "change-100", "change-101" ], change_page_tokens
      assert_equal 3, api_client.batches.length
      assert_equal [ "doc-123" ], api_client.batches.first[:files].pluck(:file_id)
      assert_equal(
        [ { broker_credential_id: credential.oid, observed_file_id: "doc-removed" } ],
        api_client.batches.second[:observation_deactivations]
      )
      refute api_client.batches.first.key?(:checkpoint)
      refute api_client.batches.second.key?(:checkpoint)
      checkpoint = api_client.batches.last.fetch(:checkpoint)
      assert_equal "change-200", checkpoint[:changes_page_token]
      assert checkpoint[:last_incremental_sync_at].present?
      assert_enqueued_with(job: FetchDocumentJob, args: [ credential.id, file ])
    end

    test "a rejected user Changes token clears the checkpoint and restarts initial sync" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(credential, user_changes_page_token: "change-expired")
      )
      google_http = lambda do |endpoint:, **|
        assert_equal SyncCredential::CHANGES_LIST_ENDPOINT, endpoint
        raise SyncCredential::InvalidPageTokenError, "Page token expired"
      end

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_equal "", api_client.checkpoint["changes_page_token"]
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
    end

    test "incremental sync without a user checkpoint hands off to initial sync" do
      credential = create_credential
      api_client = FakeApiClient.new
      google_http = ->(**) { flunk "a routing job should not call Google" }

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_empty api_client.batches
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
    end

    test "document fetch skips a canonical version that is already present" do
      credential = create_credential
      api_client = FakeApiClient.new(missing: [])
      google_http = ->(**) { flunk "documents.get should not be called" }

      with_clients(api_client, google_http) do
        FetchDocumentJob.perform_now(credential.id, google_doc)
      end

      assert_empty api_client.batches
    end

    test "document fetch ingests a missing canonical version" do
      credential = create_credential
      api_client = FakeApiClient.new
      google_http = lambda do |endpoint:, **|
        assert_equal "#{SyncCredential::DOCS_GET_ENDPOINT}/doc-123", endpoint
        {
          "title" => "Launch Plan",
          "body" => {
            "content" => [
              {
                "paragraph" => {
                  "elements" => [ { "textRun" => { "content" => "Ship it.\n" } } ]
                }
              }
            ]
          }
        }
      end

      with_clients(api_client, google_http) do
        FetchDocumentJob.perform_now(credential.id, google_doc)
      end

      batch = api_client.batches.fetch(0)
      assert_equal "doc-123", batch[:contents].first[:file_id]
      assert_equal "Ship it.\n", batch[:contents].first[:text_content]
      assert_equal "google_docs:doc-123:chunk-0000", batch[:context_documents].first[:document_id]
    end

    test "document fetch retries when the Centaur API refuses the connection" do
      credential = create_credential
      api_client = Object.new
      api_client.define_singleton_method(:get_google_docs_content_status) do |files:|
        raise Errno::ECONNREFUSED
      end

      assert_enqueued_with(job: FetchDocumentJob, args: [ credential.id, google_doc ]) do
        with_clients(api_client, ->(**) { flunk "documents.get should not be called" }) do
          FetchDocumentJob.perform_now(credential.id, google_doc)
        end
      end
    end

    test "credential crawler jobs block conflicts for the full crawl" do
      assert_equal "google_docs", InitialSyncJob.queue_name
      assert_equal InitialSyncJob.queue_name, IncrementalSyncJob.queue_name
      assert_equal InitialSyncJob.queue_name, FetchDocumentJob.queue_name
      assert_equal "GoogleDocsCredentialSync", InitialSyncJob.concurrency_group
      assert_equal InitialSyncJob.concurrency_group, IncrementalSyncJob.concurrency_group
      assert_equal :block, InitialSyncJob.concurrency_on_conflict
      assert_equal 1.hour, InitialSyncJob.concurrency_duration
    end

    private

    def with_sync_enabled(value)
      previous = ENV[SYNC_ENABLED_ENV]
      ENV[SYNC_ENABLED_ENV] = value
      yield
    ensure
      previous.nil? ? ENV.delete(SYNC_ENABLED_ENV) : ENV[SYNC_ENABLED_ENV] = previous
    end

    def with_clients(api_client, google_http)
      previous_http = SyncCredential.google_api_http
      SyncCredential.google_api_http = google_http
      CentaurApiClient.stub(:new, api_client) { yield }
    ensure
      SyncCredential.google_api_http = previous_http
    end

    def checkpoint_for(credential, user_changes_page_token:)
      {
        "broker_credential_id" => credential.oid,
        "changes_page_token" => user_changes_page_token,
        "metadata" => {}
      }
    end

    def google_doc(id = "doc-123")
      {
        "id" => id,
        "name" => "Launch Plan",
        "mimeType" => SyncCredential::GOOGLE_DOC_MIME_TYPE,
        "webViewLink" => "https://docs.google.com/document/d/#{id}/edit",
        "owners" => [ { "permissionId" => "owner-1", "displayName" => "Alice" } ],
        "capabilities" => { "canEdit" => true },
        "trashed" => false,
        "createdTime" => "2026-06-01T12:00:00Z",
        "modifiedTime" => "2026-06-02T12:00:00Z",
        "version" => "7"
      }
    end
  end
end
