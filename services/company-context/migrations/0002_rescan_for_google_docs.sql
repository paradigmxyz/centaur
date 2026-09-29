-- Existing checkpoints completed a PDF-only initial scan. Restart each corpus
-- from a fresh point-in-time token so pre-existing Google Docs are discovered
-- before its change feed resumes.
update company_context_system.google_drive_checkpoints
set initial_start_page_token = '',
    initial_page_token = '',
    initial_scan_completed = false,
    changes_page_token = '',
    last_error = '',
    updated_at = now();
