module GoogleDocs
  class InitialSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint)
      if high_water_mark(checkpoint)
        schedule(GoogleDocs::IncrementalSyncJob, credential.id)
        return
      end

      start_page_token = sync.start_page_token
      files = list_all_files(sync)
      run_id = ingest_page(
        credential,
        sync,
        files: files,
        deactivations: [],
        mode: "initial",
        source: "drive.files.list",
        replace_observation_credentials: [ credential.oid ]
      )
      enqueue_content_fetches(credential, files)
      persist_checkpoint(
        credential,
        changes_page_token: start_page_token,
        run_id: run_id,
        full_sync_finished: true
      )
      schedule(GoogleDocs::IncrementalSyncJob, credential.id)
    end

    def list_all_files(sync)
      files = []
      page_token = nil
      loop do
        page = sync.list_files_page(page_token: page_token)
        files.concat(Array(page["files"]).select { |file| sync.eligible_file?(file) })
        page_token = page["nextPageToken"].presence
        break unless page_token
      end
      files
    end
  end
end
