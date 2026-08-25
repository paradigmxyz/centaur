module GoogleDocs
  class IncrementalSyncJob < SyncJob
    def perform(credential_id)
      credential = eligible_credential(credential_id)
      return unless credential

      checkpoint = load_checkpoint(credential)
      if %w[pending listing].include?(checkpoint_phase(checkpoint))
        schedule(GoogleDocs::InitialSyncJob, credential.id)
        return
      end
      unless checkpoint&.fetch("changes_page_token", nil).present?
        restart_initial_sync(credential, checkpoint)
        return
      end

      sync = sync_client(credential)
      page = sync.list_changes_page(page_token: checkpoint.fetch("changes_page_token"))
      files, deactivations = normalize_changes(sync, page["changes"])
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
    rescue GoogleDocs::SyncCredential::InvalidPageTokenError
      restart_initial_sync(credential, checkpoint)
    end

    private

    def normalize_changes(sync, changes)
      latest_changes = Array(changes).each_with_object({}) do |change, latest|
        file_id = change["fileId"].presence || change.dig("file", "id").presence
        latest[file_id] = change if file_id
      end
      files = []
      deactivations = []
      latest_changes.each do |file_id, change|
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
