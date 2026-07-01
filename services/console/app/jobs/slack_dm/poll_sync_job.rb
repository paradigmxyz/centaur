module SlackDm
  class PollSyncJob < ApplicationJob
    queue_as :default

    def perform(oauth_app_slug = SlackDm::SyncCredential.oauth_app_slug)
      credentials = BrokerCredential
        .includes(:oauth_app)
        .joins(:oauth_app)
        .where(dead: false)
        .where(oauth_apps: {
          provider: Oauth::Providers::Slack::KEY,
          slug: oauth_app_slug,
          enabled: true
        })

      credentials.find_each do |credential|
        next if credential.access_token.blank?
        next unless SlackDm::SyncCredential.required_scopes_granted?(credential.scopes)

        SlackDm::SyncCredentialJob.perform_later(credential.id)
      end
    end
  end
end
