module GoogleDocs
  class SyncCredentialJob < ApplicationJob
    queue_as :default

    limits_concurrency to: 1, key: ->(credential_id) { "google_docs_sync_#{credential_id}" }

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless credential
      return if credential.dead?
      return if credential.access_token.blank?
      return unless credential.oauth_app&.provider == Oauth::Providers::Google::KEY
      return unless GoogleDocs::SyncCredential.required_scopes_granted?(credential.scopes)

      GoogleDocs::SyncCredential.new(credential).call
    end
  end
end
