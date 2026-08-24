module GoogleDocs
  # Compatibility router for jobs enqueued before the paginated sync rollout.
  class SyncCredentialJob < ApplicationJob
    queue_as :default

    def perform(credential_id)
      credential = BrokerCredential.find_by(id: credential_id)
      return unless credential

      checkpoint = CentaurApiClient.new
        .get_google_docs_sync_checkpoint(broker_credential_id: credential.oid)
        .fetch("checkpoint")
      phase = checkpoint&.dig("metadata", "phase")
      job_class = %w[catching_up ready].include?(phase) ? IncrementalSyncJob : InitialSyncJob
      job_class.perform_later(credential.id)
    end
  end
end
