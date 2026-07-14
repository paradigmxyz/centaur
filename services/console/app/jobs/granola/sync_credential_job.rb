module Granola
  class SyncCredentialJob < ApplicationJob
    queue_as :default

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless Granola::SyncCredential.syncable?(credential)

      Granola::SyncCredential.new(credential).call
    end
  end
end
