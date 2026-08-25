module GoogleDocs
  class PollSyncJob < ApplicationJob
    queue_as :default

    def perform(oauth_app_slug = GoogleDocs::SyncCredential.oauth_app_slug)
      api_client = CentaurApiClient.new
      credentials = BrokerCredential
        .includes(:oauth_app)
        .joins(:oauth_app)
        .where(dead: false)
        .where(oauth_apps: {
          provider: Oauth::Providers::Google::KEY,
          slug: oauth_app_slug,
          enabled: true
        })

      credentials.find_each do |credential|
        next unless GoogleDocs::SyncCredential.syncable?(credential, oauth_app_slug: oauth_app_slug)

        sync_state = api_client
          .get_google_docs_sync_checkpoint(broker_credential_id: credential.oid)
        checkpoint = sync_state.fetch("checkpoint")
        high_water_mark = SyncJob.high_water_mark(checkpoint)
        next if !high_water_mark && sync_state["initial_sync_active"] == true

        job_class = high_water_mark ? IncrementalSyncJob : InitialSyncJob
        job_class.perform_later(credential.id)
      rescue CentaurApiClient::Error => e
        Rails.logger.warn do
          "Google Docs poll failed to load checkpoint for credential #{credential.id}: " \
            "#{e.class}: #{e.message}"
        end
      end
    end
  end
end
