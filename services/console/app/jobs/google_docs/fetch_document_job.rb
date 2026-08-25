module GoogleDocs
  class FetchDocumentJob < BaseJob
    CONCURRENCY_DURATION = 10.minutes

    limits_concurrency(
      to: 1,
      key: lambda { |_credential_id, file|
        file_id = file["id"] || file[:id]
        "google_docs_content_#{file_id}"
      },
      group: "GoogleDocsContentFetch",
      duration: CONCURRENCY_DURATION
    )

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
