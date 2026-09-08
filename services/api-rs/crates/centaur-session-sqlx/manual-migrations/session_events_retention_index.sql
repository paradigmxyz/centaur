-- Run separately from application startup, outside a transaction.
-- If an earlier build failed, drop or repair the invalid index before retrying.
create index concurrently session_events_stdout_created_at_idx
    on session_events (created_at)
    where event_type = 'session.output.line';
