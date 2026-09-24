module GoogleDocs
  class PdfBackfillJob < SyncJob
    private

    def sync_page(credential, sync, checkpoint)
      unless user_changes_page_token(checkpoint)
        GoogleDocs::InitialSyncJob.perform_later(credential.id)
        return
      end
      unless Config.pdf_indexing_enabled?
        reset_backfill_marker(credential, checkpoint)
        GoogleDocs::IncrementalSyncJob.perform_later(credential.id)
        return
      end
      unless SyncCredential.pdf_backfill_required?(credential, checkpoint)
        GoogleDocs::IncrementalSyncJob.perform_later(credential.id)
        return
      end

      run_id = "gdocs_pdf_backfill_#{SecureRandom.hex(16)}"
      page_token = nil
      files_seen = 0
      loop do
        page = sync.list_user_pdfs_page(page_token: page_token)
        files = Array(page["files"]).select { |file| sync.eligible_file?(file) }
        files_seen += files.length
        ingest_page(
          credential,
          sync,
          files: files,
          deactivations: [],
          mode: "pdf_backfill",
          source: "drive.files.list.pdf_backfill",
          run_id: run_id,
          files_seen: files_seen,
          finished: false
        )
        enqueue_content_fetches(credential, files)

        page_token = page["nextPageToken"].presence
        break unless page_token
      end

      api_client.ingest_google_docs_sync_batch(
        run: run_payload(
          credential,
          run_id,
          mode: "pdf_backfill",
          files_seen: files_seen,
          finished: true
        ),
        checkpoint: checkpoint_payload(
          credential,
          user_changes_page_token: user_changes_page_token(checkpoint),
          run_id: run_id,
          pdf_backfill_version: SyncCredential::PDF_BACKFILL_VERSION,
          metadata: checkpoint.to_h.fetch("metadata", {})
        ),
        replace_context_documents: false
      )
      GoogleDocs::IncrementalSyncJob.perform_later(credential.id)
    end

    def reset_backfill_marker(credential, checkpoint)
      return unless SyncCredential.pdf_backfill_reset_required?(checkpoint)

      api_client.ingest_google_docs_sync_batch(
        checkpoint: checkpoint_payload(
          credential,
          user_changes_page_token: user_changes_page_token(checkpoint),
          run_id: checkpoint["last_run_id"],
          pdf_backfill_version: 0,
          metadata: checkpoint.to_h.fetch("metadata", {})
        ),
        replace_context_documents: false
      )
    end
  end
end
