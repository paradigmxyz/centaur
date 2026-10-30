-- Run with company-context workers stopped to force a fresh metadata scan of
-- every user and Shared Drive corpus. Unchanged files retain their observation
-- keys, so the scan does not enqueue them for extraction again.
--
-- Completed scans resume their change feed from the last processed token, so
-- deletions and unshares made before the rescan finishes are still applied.
update company_context_system.google_drive_checkpoints
set initial_start_page_token = case
        when initial_scan_completed then changes_page_token
        else initial_start_page_token
    end,
    initial_page_token = '',
    initial_scan_completed = false,
    last_error = '',
    updated_at = now();
