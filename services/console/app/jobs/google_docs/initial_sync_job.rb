module GoogleDocs
  class InitialSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint)
      if self.class.job_class_for(checkpoint) == IncrementalSyncJob
        if checkpoint&.fetch("changes_page_token", nil).present?
          schedule(GoogleDocs::IncrementalSyncJob, credential.id)
        else
          restart_initial_sync(credential, checkpoint)
        end
        return
      end
      unless checkpoint_phase(checkpoint) == "listing" && initial_crawl_id(checkpoint)
        checkpoint = initialize_crawl(credential, sync, checkpoint)
      end
      crawl_id = initial_crawl_id(checkpoint)
      page = sync.list_files_page(page_token: initial_page_token(checkpoint))
      files = Array(page["files"]).select { |file| sync.eligible_file?(file) }
      run_id = new_run_id
      next_page_token = page["nextPageToken"].presence
      next_phase = next_page_token ? "listing" : "catching_up"
      ingest_page(
        credential,
        sync,
        run_id: run_id,
        files: files,
        deactivations: [],
        mode: "initial",
        source: "drive.files.list",
        initial_crawl_id: crawl_id,
        observation_sweeps: next_page_token ? [] : [ {
          broker_credential_id: credential.oid,
          initial_crawl_id: crawl_id
        } ],
        checkpoint: checkpoint_payload(
          credential,
          checkpoint,
          phase: next_phase,
          initial_page_token: next_page_token,
          run_id: run_id
        )
      )

      schedule(next_page_token ? self.class : GoogleDocs::IncrementalSyncJob, credential.id)
    end

    def initialize_crawl(credential, sync, checkpoint)
      start_page_token = sync.start_page_token
      crawl_id = SecureRandom.uuid
      payload = checkpoint_payload(
        credential,
        checkpoint,
        phase: "listing",
        initial_crawl_id: crawl_id,
        start_page_token: start_page_token,
        changes_page_token: start_page_token
      )
      api_client.ingest_google_docs_sync_batch(
        checkpoint: payload,
        replace_context_documents: false
      )
      payload.deep_stringify_keys
    end
  end
end
