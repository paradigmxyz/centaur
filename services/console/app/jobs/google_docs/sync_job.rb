module GoogleDocs
  class SyncJob < BaseJob
    CONCURRENCY_DURATION = 1.hour

    limits_concurrency(
      to: 1,
      key: ->(credential_id) { "google_docs_sync_#{credential_id}" },
      group: "GoogleDocsCredentialSync",
      duration: CONCURRENCY_DURATION,
      on_conflict: :block
    )

    def self.user_changes_page_token(checkpoint)
      # The persisted field predates shared-drive scopes. It contains only the
      # credential's user change-log token.
      checkpoint&.fetch("changes_page_token", nil).presence
    end

    def perform(credential_id)
      return unless GoogleDocs::Config.sync_enabled?

      credential = eligible_credential(credential_id)
      return unless credential

      sync_page(credential, sync_client(credential), load_checkpoint(credential))
    rescue GoogleDocs::SyncCredential::InvalidPageTokenError
      restart_initial_sync(credential)
    end

    private

    def sync_page(_credential, _sync, _checkpoint)
      raise NotImplementedError
    end

    def load_checkpoint(credential)
      api_client
        .get_google_docs_sync_checkpoint(broker_credential_id: credential.oid)
        .fetch("checkpoint")
    end

    def user_changes_page_token(checkpoint)
      self.class.user_changes_page_token(checkpoint)
    end

    def checkpoint_payload(
      credential,
      user_changes_page_token:,
      run_id: nil,
      full_sync_finished: false,
      incremental_sync_finished: false
    )
      payload = {
        broker_credential_id: credential.oid,
        provider_subject: credential.provider_subject.to_s,
        provider_email: credential.provider_email.to_s,
        changes_page_token: user_changes_page_token,
        last_run_id: run_id,
        last_error: "",
        metadata: {}
      }
      now = Time.current.iso8601
      payload[:last_full_sync_at] = now if full_sync_finished
      payload[:last_incremental_sync_at] = now if incremental_sync_finished
      payload
    end

    def ingest_page(
      credential,
      sync,
      files:,
      deactivations:,
      mode:,
      source:,
      run_id: nil,
      files_seen: files.length,
      finished: true
    )
      run_id ||= "gdocs_#{SecureRandom.hex(16)}"
      api_client.ingest_google_docs_sync_batch(
        run: run_payload(
          credential,
          run_id,
          mode: mode,
          files_seen: files_seen,
          finished: finished
        ),
        files: files.map { |file| sync.file_payload(file, run_id: run_id) },
        observations: files.map do |file|
          sync.observation_payload(file, run_id: run_id, source: source)
        end,
        observation_deactivations: deactivations,
        replace_context_documents: false
      )
      run_id
    end

    def enqueue_content_fetches(credential, files)
      files.each do |file|
        GoogleDocs::FetchDocumentJob.perform_later(credential.id, file)
      end
    end

    def persist_user_checkpoint(credential, user_changes_page_token:, run_id:, **timestamps)
      api_client.ingest_google_docs_sync_batch(
        checkpoint: checkpoint_payload(
          credential,
          user_changes_page_token: user_changes_page_token,
          run_id: run_id,
          **timestamps
        ),
        replace_context_documents: false
      )
    end

    def run_payload(credential, run_id, mode:, files_seen:, finished: true)
      {
        run_id: run_id,
        mode: mode,
        status: finished ? "completed" : "running",
        broker_credential_id: credential.oid,
        provider_subject: credential.provider_subject.to_s,
        provider_email: credential.provider_email.to_s,
        files_seen: files_seen,
        files_upserted: files_seen,
        finished: finished,
        metadata: { oauth_app_slug: credential.oauth_app&.slug }
      }
    end

    def restart_initial_sync(credential)
      persist_user_checkpoint(credential, user_changes_page_token: "", run_id: nil)
      GoogleDocs::InitialSyncJob.perform_later(credential.id)
    end
  end
end
