module GoogleDocs
  class IncrementalSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint, _continuation = nil)
      page_token = high_water_mark(checkpoint)
      unless page_token
        schedule(GoogleDocs::InitialSyncJob, credential.id)
        return
      end

      loop do
        page = sync.list_changes_page(page_token: page_token)
        files, deactivations = partition_changes(sync, page["changes"])
        run_id = ingest_page(
          credential,
          sync,
          files: files,
          deactivations: deactivations,
          mode: "incremental",
          source: "drive.changes.list"
        )
        enqueue_content_fetches(credential, files)

        page_token = page["nextPageToken"].presence
        next if page_token

        new_start_page_token = page["newStartPageToken"].presence
        unless new_start_page_token
          raise GoogleDocs::SyncCredential::GoogleApiError,
            "Google Drive returned no new start page token"
        end
        persist_checkpoint(
          credential,
          changes_page_token: new_start_page_token,
          run_id: run_id,
          incremental_sync_finished: true
        )
        break
      end
    end

    def partition_changes(sync, changes)
      files = []
      deactivations = []
      Array(changes).each do |change|
        file_id = change["fileId"].presence || change.dig("file", "id").presence
        next unless file_id

        file = change["file"]
        if change["removed"] == true || !sync.eligible_file?(file)
          deactivations << sync.observation_deactivation(file_id)
        else
          files << file
        end
      end
      [ files, deactivations ]
    end
  end
end
