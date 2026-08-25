require "test_helper"

module GoogleDocs
  class JobsTest < ActiveJob::TestCase
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

    test "poll job enqueues the incremental entry job for a Drive readonly credential" do
      app = create_google_app
      credential = create_credential(app: app)

      PollSyncJob.perform_now(app.slug)

      assert_enqueued_with(job: IncrementalSyncJob, args: [ credential.id ])
    end

    test "initial sync captures a change token and marks observations without resetting visibility" do
      credential = create_credential
      api_client = FakeApiClient.new
      file = google_doc
      google_http = lambda do |endpoint:, params:, access_token:|
        assert_equal "token", access_token
        case endpoint
        when SyncCredential::START_PAGE_TOKEN_ENDPOINT
          { "startPageToken" => "change-100" }
        when SyncCredential::FILES_LIST_ENDPOINT
          assert_nil params["pageToken"]
          { "files" => [ file ], "nextPageToken" => "files-2" }
        else
          flunk "unexpected Google endpoint #{endpoint}"
        end
      end

      with_clients(api_client, google_http) do
        InitialSyncJob.perform_now(credential.id)
      end

      initialization = api_client.batches.first
      assert_equal "change-100", initialization.dig(:checkpoint, :start_page_token)
      assert_equal "change-100", initialization.dig(:checkpoint, :changes_page_token)
      crawl_id = initialization.dig(:checkpoint, :metadata, "initial_crawl_id")
      assert crawl_id.present?
      metadata_batch = api_client.batches.second
      assert_equal "completed", metadata_batch.dig(:run, :status)
      assert metadata_batch[:checkpoint]
      assert_equal [ "doc-123" ], metadata_batch[:files].pluck(:file_id)
      assert_equal [ "doc-123" ], metadata_batch[:observations].pluck(:observed_file_id)
      assert_equal crawl_id, metadata_batch[:observations].first.dig(:raw_payload, "initial_crawl_id")
      assert_equal "files-2", api_client.checkpoint.dig("metadata", "initial_page_token")
      assert_equal "listing", api_client.checkpoint.dig("metadata", "phase")
      assert_empty api_client.batches.last[:observation_sweeps]
      assert_enqueued_with(job: FetchDocumentJob, args: [ credential.id, file ])
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
    end

    test "initial sync hands its baseline token to incremental catch-up after the final page" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "listing",
          changes_page_token: "change-100",
          initial_page_token: "files-2",
          initial_crawl_id: "crawl-123"
        )
      )
      google_http = lambda do |endpoint:, params:, **|
        assert_equal SyncCredential::FILES_LIST_ENDPOINT, endpoint
        assert_equal "files-2", params["pageToken"]
        { "files" => [] }
      end

      with_clients(api_client, google_http) do
        InitialSyncJob.perform_now(credential.id)
      end

      assert_equal "catching_up", api_client.checkpoint.dig("metadata", "phase")
      assert_equal "change-100", api_client.checkpoint["changes_page_token"]
      assert_equal(
        [ { broker_credential_id: credential.oid, initial_crawl_id: "crawl-123" } ],
        api_client.batches.last[:observation_sweeps]
      )
      assert_enqueued_with(job: IncrementalSyncJob, args: [ credential.id ])
    end

    test "a rejected initial page token resets the checkpoint and restarts the initial crawl" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "listing",
          changes_page_token: "change-100",
          initial_page_token: "files-expired",
          initial_crawl_id: "crawl-123"
        )
      )
      google_http = lambda do |endpoint:, **|
        assert_equal SyncCredential::FILES_LIST_ENDPOINT, endpoint
        raise SyncCredential::InvalidPageTokenError, "Page token expired"
      end

      with_clients(api_client, google_http) do
        InitialSyncJob.perform_now(credential.id)
      end

      assert_equal "pending", api_client.checkpoint.dig("metadata", "phase")
      assert_equal "", api_client.checkpoint.dig("metadata", "initial_page_token")
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
    end

    test "incremental sync applies current files and removals before advancing its token" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "catching_up",
          changes_page_token: "change-100"
        )
      )
      file = google_doc
      google_http = lambda do |endpoint:, params:, **|
        assert_equal SyncCredential::CHANGES_LIST_ENDPOINT, endpoint
        assert_equal "change-100", params["pageToken"]
        {
          "changes" => [
            { "fileId" => "doc-removed", "removed" => true },
            { "fileId" => "doc-123", "file" => file }
          ],
          "newStartPageToken" => "change-200"
        }
      end

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      metadata_batch = api_client.batches.first
      assert_equal 1, api_client.batches.length
      assert_equal "completed", metadata_batch.dig(:run, :status)
      assert_equal [ "doc-123" ], metadata_batch[:files].pluck(:file_id)
      assert_equal(
        [ { broker_credential_id: credential.oid, observed_file_id: "doc-removed" } ],
        metadata_batch[:observation_deactivations]
      )
      assert_equal "ready", api_client.checkpoint.dig("metadata", "phase")
      assert_equal "change-200", api_client.checkpoint["changes_page_token"]
      assert api_client.checkpoint["last_full_sync_at"].present?
      assert api_client.checkpoint["last_incremental_sync_at"].present?
      assert_no_enqueued_jobs(only: IncrementalSyncJob)
    end

    test "incremental sync persists each change page before scheduling the next page" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "ready",
          changes_page_token: "change-200"
        )
      )
      google_http = lambda do |endpoint:, params:, **|
        assert_equal SyncCredential::CHANGES_LIST_ENDPOINT, endpoint
        assert_equal "change-200", params["pageToken"]
        { "changes" => [], "nextPageToken" => "change-201" }
      end

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_equal "ready", api_client.checkpoint.dig("metadata", "phase")
      assert_equal "change-201", api_client.checkpoint["changes_page_token"]
      assert_nil api_client.checkpoint["last_incremental_sync_at"]
      assert_enqueued_with(job: IncrementalSyncJob, args: [ credential.id ])
    end

    test "a rejected changes token resets the checkpoint and restarts the initial crawl" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "ready",
          changes_page_token: "change-expired"
        )
      )
      google_http = lambda do |endpoint:, **|
        assert_equal SyncCredential::CHANGES_LIST_ENDPOINT, endpoint
        raise SyncCredential::InvalidPageTokenError, "Page token expired"
      end

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_equal "pending", api_client.checkpoint.dig("metadata", "phase")
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
    end

    test "a missing changes token resets the checkpoint instead of bouncing between crawlers" do
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "ready",
          changes_page_token: ""
        )
      )
      google_http = ->(**) { flunk "a missing token should not call Google" }

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_equal "pending", api_client.checkpoint.dig("metadata", "phase")
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])
      assert_no_enqueued_jobs(only: IncrementalSyncJob)
    end

    test "crawler entry jobs hand off without moving a checkpoint backward" do
      credential = create_credential
      api_client = FakeApiClient.new
      google_http = ->(**) { flunk "a routing job should not call Google" }

      with_clients(api_client, google_http) do
        IncrementalSyncJob.perform_now(credential.id)
      end

      assert_empty api_client.batches
      assert_enqueued_with(job: InitialSyncJob, args: [ credential.id ])

      clear_enqueued_jobs
      credential = create_credential
      api_client = FakeApiClient.new(
        checkpoint: checkpoint_for(
          credential,
          phase: "ready",
          changes_page_token: "change-200"
        )
      )
      with_clients(api_client, google_http) do
        InitialSyncJob.perform_now(credential.id)
      end

      assert_empty api_client.batches
      assert_equal "change-200", api_client.checkpoint["changes_page_token"]
      assert_enqueued_with(job: IncrementalSyncJob, args: [ credential.id ])
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

    test "credential crawler jobs share a long-lived discard concurrency group" do
      assert_equal "GoogleDocsCredentialSync", InitialSyncJob.concurrency_group
      assert_equal InitialSyncJob.concurrency_group, IncrementalSyncJob.concurrency_group
      assert_equal :discard, InitialSyncJob.concurrency_on_conflict
      assert_equal 1.hour, InitialSyncJob.concurrency_duration
    end

    private

    def with_clients(api_client, google_http)
      previous_http = SyncCredential.google_api_http
      SyncCredential.google_api_http = google_http
      CentaurApiClient.stub(:new, api_client) { yield }
    ensure
      SyncCredential.google_api_http = previous_http
    end

    def checkpoint_for(
      credential,
      phase:,
      changes_page_token:,
      initial_page_token: nil,
      initial_crawl_id: nil
    )
      {
        "broker_credential_id" => credential.oid,
        "start_page_token" => "change-100",
        "changes_page_token" => changes_page_token,
        "metadata" => {
          "phase" => phase,
          "initial_page_token" => initial_page_token.to_s,
          "initial_crawl_id" => initial_crawl_id
        }
      }
    end

    def google_doc
      {
        "id" => "doc-123",
        "name" => "Launch Plan",
        "mimeType" => SyncCredential::GOOGLE_DOC_MIME_TYPE,
        "webViewLink" => "https://docs.google.com/document/d/doc-123/edit",
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
