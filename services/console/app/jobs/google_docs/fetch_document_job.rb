module GoogleDocs
  class FetchDocumentJob < ApplicationJob
    CONCURRENCY_DURATION = 10.minutes

    queue_as :default

    limits_concurrency(
      to: 1,
      key: lambda { |_credential_id, file|
        file_id = file["id"] || file[:id]
        "google_docs_content_#{file_id}"
      },
      group: "GoogleDocsContentFetch",
      duration: CONCURRENCY_DURATION
    )

    retry_on GoogleDocs::SyncCredential::GoogleApiError,
      CentaurApiClient::Error,
      wait: :polynomially_longer,
      attempts: 5

    def perform(credential_id, file)
      credential = eligible_credential(credential_id)
      return unless credential

      sync = GoogleDocs::SyncCredential.new(credential)
      version = sync.content_version(file)
      api_client = CentaurApiClient.new
      missing = api_client
        .get_google_docs_content_status(files: [ version ])
        .fetch("missing")
      return if Array(missing).empty?

      api_client.ingest_google_docs_sync_batch(sync.document_batch(file))
    end

    private

    def eligible_credential(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless credential
      return if credential.dead? || credential.access_token.blank?
      return unless credential.oauth_app&.provider == Oauth::Providers::Google::KEY
      return unless GoogleDocs::SyncCredential.required_scopes_granted?(credential.scopes)

      credential
    end
  end
end
