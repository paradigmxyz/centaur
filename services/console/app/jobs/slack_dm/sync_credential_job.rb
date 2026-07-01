module SlackDm
  class SyncCredentialJob < ApplicationJob
    queue_as :default

    limits_concurrency to: 1, key: ->(credential_id) { "slack_dm_sync_#{credential_id}" }

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless credential
      return if credential.dead?
      return if credential.access_token.blank?
      return unless credential.oauth_app&.provider == Oauth::Providers::Slack::KEY
      return unless SlackDm::SyncCredential.required_scopes_granted?(credential.scopes)

      SlackDm::SyncCredential.new(credential).call
    end
  end
end
