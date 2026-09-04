module Granola
  class SyncCredentialJob < ApplicationJob
    queue_as :default

    retry_on Errno::ECONNREFUSED, wait: :polynomially_longer, attempts: 5

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless Granola::SyncCredential.syncable?(credential)

      Granola::SyncCredential.new(credential).call
    end
  end
end
