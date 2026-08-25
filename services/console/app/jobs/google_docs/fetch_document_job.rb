module GoogleDocs
  class FetchDocumentJob < BaseJob
    def perform(credential_id, file)
      credential = eligible_credential(credential_id)
      return unless credential

      sync = sync_client(credential)
      version = sync.content_version(file)
      missing = api_client
        .get_google_docs_content_status(files: [ version ])
        .fetch("missing")
      return if Array(missing).empty?

      api_client.ingest_google_docs_sync_batch(sync.document_batch(file))
    end
  end
end
