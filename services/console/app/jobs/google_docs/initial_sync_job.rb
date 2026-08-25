module GoogleDocs
  class InitialSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint)
      return if user_changes_page_token(checkpoint)

      user_start_page_token = sync.user_start_page_token
      run_id = "gdocs_#{SecureRandom.hex(16)}"
      page_token = nil
      files_seen = 0
      loop do
        page = sync.list_user_files_page(page_token: page_token)
        files = Array(page["files"]).select { |file| sync.eligible_file?(file) }
        files_seen += files.length
        ingest_page(
          credential,
          sync,
          files: files,
          deactivations: [],
          mode: "initial",
          source: "drive.files.list",
          run_id: run_id,
          files_seen: files_seen,
          finished: false
        )
        enqueue_content_fetches(credential, files)

        page_token = page["nextPageToken"].presence
        break unless page_token
      end

      api_client.ingest_google_docs_sync_batch(
        run: run_payload(
          credential,
          run_id,
          mode: "initial",
          files_seen: files_seen,
          finished: true
        ),
        observation_sweeps: [
          { broker_credential_id: credential.oid, source_run_id: run_id }
        ],
        checkpoint: checkpoint_payload(
          credential,
          user_changes_page_token: user_start_page_token,
          run_id: run_id,
          full_sync_finished: true
        ),
        replace_context_documents: false
      )
      GoogleDocs::IncrementalSyncJob.perform_later(credential.id)
    end
  end
end
