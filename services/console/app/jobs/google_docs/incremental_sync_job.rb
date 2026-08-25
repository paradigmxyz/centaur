module GoogleDocs
  class IncrementalSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint)
      page_token = user_changes_page_token(checkpoint)
      unless page_token
        GoogleDocs::InitialSyncJob.perform_later(credential.id)
        return
      end

      loop do
        page = sync.list_user_changes_page(page_token: page_token)
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

        next_user_changes_page_token = page["newStartPageToken"].presence
        unless next_user_changes_page_token
          raise GoogleDocs::SyncCredential::GoogleApiError,
            "Google Drive returned no new start page token"
        end
        persist_user_checkpoint(
          credential,
          user_changes_page_token: next_user_changes_page_token,
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
        # User change logs contain Shared Drive membership events without a
        # file ID. A future drive-scoped sync will process those drives and
        # their independent change logs.
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
