module Granola
  class PollSyncJob < ApplicationJob
    queue_as :default

    def perform(oauth_app_slug = Granola::SyncCredential.oauth_app_slug)
      credentials = BrokerCredential
        .includes(:oauth_app)
        .joins(:oauth_app)
        .where(dead: false)
        .where(oauth_apps: {
          provider: Oauth::Providers::Granola::KEY,
          slug: oauth_app_slug,
          enabled: true
        })

      credentials.find_each do |credential|
        next unless Granola::SyncCredential.syncable?(credential, oauth_app_slug: oauth_app_slug)

        Granola::SyncCredentialJob.perform_later(credential.id)
      end
    end
  end
end
