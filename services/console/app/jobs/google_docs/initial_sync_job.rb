module GoogleDocs
  class InitialSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint, continuation = nil)
      if high_water_mark(checkpoint)
        schedule(GoogleDocs::IncrementalSyncJob, credential.id)
        return
      end

      state = continuation_state(sync, continuation)
      page = sync.list_files_page(page_token: state["page_token"].presence)
      files = Array(page["files"]).select { |file| sync.eligible_file?(file) }
      files_seen = state.fetch("files_seen").to_i + files.length
      ingest_page(
        credential,
        sync,
        files: files,
        deactivations: [],
        mode: "initial",
        source: "drive.files.list",
        run_id: state.fetch("run_id"),
        files_seen: files_seen,
        finished: false
      )
      enqueue_content_fetches(credential, files)

      next_page_token = page["nextPageToken"].presence
      if next_page_token
        schedule(
          GoogleDocs::InitialSyncJob,
          credential.id,
          state.merge("page_token" => next_page_token, "files_seen" => files_seen)
        )
      else
        complete_crawl(credential, state, files_seen)
      end
    end

    def continuation_state(sync, continuation)
      return continuation.stringify_keys if continuation

      {
        "run_id" => "gdocs_#{SecureRandom.hex(16)}",
        "start_page_token" => sync.start_page_token,
        "page_token" => nil,
        "files_seen" => 0
      }
    end

    def complete_crawl(credential, state, files_seen)
      run_id = state.fetch("run_id")
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
          changes_page_token: state.fetch("start_page_token"),
          run_id: run_id,
          full_sync_finished: true
        ),
        replace_context_documents: false
      )
      schedule(GoogleDocs::IncrementalSyncJob, credential.id)
    end
  end
end
