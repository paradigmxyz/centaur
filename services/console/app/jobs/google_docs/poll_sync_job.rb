module GoogleDocs
  class PollSyncJob < ApplicationJob
    queue_as :default

    def perform(oauth_app_slug = GoogleDocs::SyncCredential.oauth_app_slug)
      return unless GoogleDocs::Config.sync_enabled?

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

        checkpoint = api_client
          .get_google_docs_sync_checkpoint(broker_credential_id: credential.oid)
          .fetch("checkpoint")
        job_class = if !SyncJob.user_changes_page_token(checkpoint)
          InitialSyncJob
        elsif SyncCredential.pdf_backfill_reset_required?(checkpoint) ||
            SyncCredential.pdf_backfill_required?(credential, checkpoint)
          PdfBackfillJob
        else
          IncrementalSyncJob
        end
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
