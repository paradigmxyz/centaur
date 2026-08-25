module GoogleDocs
  class IncrementalSyncJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint)
      if self.class.job_class_for(checkpoint) == InitialSyncJob
        schedule(GoogleDocs::InitialSyncJob, credential.id)
        return
      end
      unless checkpoint&.fetch("changes_page_token", nil).present?
        restart_initial_sync(credential, checkpoint)
        return
      end

      page = sync.list_changes_page(page_token: checkpoint.fetch("changes_page_token"))
      files, deactivations = partition_changes(sync, page["changes"])
      run_id = ingest_metadata_page(
        credential,
        sync,
        files: files,
        deactivations: deactivations,
        mode: "incremental",
        source: "drive.changes.list"
      )
      next_page_token = page["nextPageToken"].presence
      new_start_page_token = page["newStartPageToken"].presence
      if next_page_token.nil? && new_start_page_token.nil?
        raise GoogleDocs::SyncCredential::GoogleApiError,
          "Google Drive returned neither a next page token nor a new start page token"
      end

      catching_up = checkpoint_phase(checkpoint) == "catching_up"
      finished = next_page_token.nil?
      finish_page(
        credential,
        run_id,
        mode: "incremental",
        files_seen: files.length,
        checkpoint: checkpoint_payload(
          credential,
          checkpoint,
          phase: finished ? "ready" : checkpoint_phase(checkpoint),
          changes_page_token: next_page_token || new_start_page_token,
          run_id: run_id,
          full_sync_finished: finished && catching_up,
          incremental_sync_finished: finished
        )
      )
      schedule(self.class, credential.id) if next_page_token
    end

    def partition_changes(sync, changes)
      file_changes, deactivation_changes = Array(changes).partition do |change|
        change["removed"] != true && sync.eligible_file?(change["file"])
      end
      [
        file_changes.map { |change| change.fetch("file") },
        deactivation_changes.map do |change|
          sync.observation_deactivation(change.fetch("fileId"))
        end
      ]
    end
  end
end
