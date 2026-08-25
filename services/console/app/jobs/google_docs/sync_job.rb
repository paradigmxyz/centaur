module GoogleDocs
  class SyncJob < BaseJob
    CONCURRENCY_DURATION = 1.hour
    HANDOFF_DELAY = 2.seconds
    INCREMENTAL_PHASES = %w[catching_up ready].freeze

    limits_concurrency(
      to: 1,
      key: ->(credential_id) { "google_docs_sync_#{credential_id}" },
      group: "GoogleDocsCredentialSync",
      duration: CONCURRENCY_DURATION,
      on_conflict: :discard
    )

    def self.checkpoint_phase(checkpoint)
      checkpoint&.dig("metadata", "phase").presence || "pending"
    end

    def self.job_class_for(checkpoint)
      INCREMENTAL_PHASES.include?(checkpoint_phase(checkpoint)) ? IncrementalSyncJob : InitialSyncJob
    end

    def perform(credential_id)
      credential = eligible_credential(credential_id)
      return unless credential

      sync = sync_client(credential)
      checkpoint = load_checkpoint(credential)
      sync_page(credential, sync, checkpoint)
    rescue GoogleDocs::SyncCredential::InvalidPageTokenError
      restart_initial_sync(credential, checkpoint)
    end

    private

    def sync_page(*)
      raise NotImplementedError
    end

    def load_checkpoint(credential)
      api_client
        .get_google_docs_sync_checkpoint(broker_credential_id: credential.oid)
        .fetch("checkpoint")
    end

    def checkpoint_phase(checkpoint)
      self.class.checkpoint_phase(checkpoint)
    end

    def initial_page_token(checkpoint)
      checkpoint&.dig("metadata", "initial_page_token").presence
    end

    def initial_crawl_id(checkpoint)
      checkpoint&.dig("metadata", "initial_crawl_id").presence
    end

    def checkpoint_payload(
      credential,
      checkpoint,
      phase:,
      initial_page_token: nil,
      initial_crawl_id: nil,
      start_page_token: nil,
      changes_page_token: nil,
      run_id: nil,
      full_sync_finished: false,
      incremental_sync_finished: false
    )
      metadata = checkpoint&.fetch("metadata", {})
      metadata = {} unless metadata.is_a?(Hash)
      checkpoint_metadata = metadata.merge(
        "phase" => phase,
        "initial_page_token" => initial_page_token.to_s
      )
      checkpoint_metadata["initial_crawl_id"] = initial_crawl_id if initial_crawl_id
      payload = {
        broker_credential_id: credential.oid,
        provider_subject: credential.provider_subject.to_s,
        provider_email: credential.provider_email.to_s,
        last_run_id: run_id,
        last_error: "",
        metadata: checkpoint_metadata
      }
      payload[:start_page_token] = start_page_token if start_page_token
      payload[:changes_page_token] = changes_page_token if changes_page_token
      now = Time.current.iso8601
      payload[:last_full_sync_at] = now if full_sync_finished
      payload[:last_incremental_sync_at] = now if incremental_sync_finished
      payload
    end

    def ingest_metadata_page(
      credential,
      sync,
      files:,
      deactivations:,
      mode:,
      source:,
      initial_crawl_id: nil
    )
      run_id = "gdocs_#{SecureRandom.hex(16)}"
      api_client.ingest_google_docs_sync_batch(
        run: run_payload(
          credential,
          run_id,
          mode: mode,
          status: "running",
          files_seen: files.length
        ),
        files: files.map { |file| sync.file_payload(file, run_id: run_id) },
        observations: files.map do |file|
          sync.observation_payload(
            file,
            run_id: run_id,
            source: source,
            initial_crawl_id: initial_crawl_id
          )
        end,
        observation_deactivations: deactivations,
        replace_context_documents: false
      )
      enqueue_missing_content(credential, sync, files)
      run_id
    end

    def finish_page(credential, run_id, mode:, files_seen:, checkpoint:, observation_sweeps: [])
      api_client.ingest_google_docs_sync_batch(
        run: run_payload(
          credential,
          run_id,
          mode: mode,
          status: "completed",
          files_seen: files_seen
        ),
        observation_sweeps: observation_sweeps,
        checkpoint: checkpoint,
        replace_context_documents: false
      )
    end

    def enqueue_missing_content(credential, sync, files)
      return if files.empty?

      versions = files.map { |file| sync.content_version(file) }
      missing = api_client.get_google_docs_content_status(files: versions).fetch("missing")
      missing_versions = Array(missing).to_h do |file|
        [ [ file.fetch("file_id"), file.fetch("source_version", "") ], true ]
      end
      files.each do |file|
        version = sync.content_version(file)
        key = [ version.fetch(:file_id), version.fetch(:source_version) ]
        next unless missing_versions[key]

        GoogleDocs::FetchDocumentJob.perform_later(credential.id, file)
      end
    end

    def run_payload(credential, run_id, mode:, status:, files_seen:)
      {
        run_id: run_id,
        mode: mode,
        status: status,
        broker_credential_id: credential.oid,
        provider_subject: credential.provider_subject.to_s,
        provider_email: credential.provider_email.to_s,
        files_seen: files_seen,
        files_upserted: files_seen,
        finished: status == "completed",
        metadata: { oauth_app_slug: credential.oauth_app&.slug }
      }
    end

    def schedule(job_class, credential_id)
      job_class.set(wait: HANDOFF_DELAY).perform_later(credential_id)
    end

    def restart_initial_sync(credential, checkpoint)
      api_client.ingest_google_docs_sync_batch(
        checkpoint: checkpoint_payload(
          credential,
          checkpoint,
          phase: "pending"
        ),
        replace_context_documents: false
      )
      schedule(GoogleDocs::InitialSyncJob, credential.id)
    end
  end
end
