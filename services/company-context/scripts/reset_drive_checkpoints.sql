-- Run with company-context workers stopped to force a fresh metadata scan of
-- every user and Shared Drive corpus. Unchanged files retain their observation
-- keys, so the scan does not enqueue them for extraction again.
update company_context_system.google_drive_checkpoints
set initial_start_page_token = '',
    initial_page_token = '',
    initial_scan_completed = false,
    changes_page_token = '',
    last_error = '',
    updated_at = now();
