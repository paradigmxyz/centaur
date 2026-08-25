module GoogleDocs
  class BaseJob < ApplicationJob
    queue_as :google_docs

    retry_on GoogleDocs::SyncCredential::GoogleApiError,
      CentaurApiClient::Error,
      wait: :polynomially_longer,
      attempts: 5

    private

    def eligible_credential(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless GoogleDocs::SyncCredential.syncable?(credential)

      credential
    end

    def api_client
      @api_client ||= CentaurApiClient.new
    end

    def sync_client(credential)
      GoogleDocs::SyncCredential.new(credential)
    end
  end
end
