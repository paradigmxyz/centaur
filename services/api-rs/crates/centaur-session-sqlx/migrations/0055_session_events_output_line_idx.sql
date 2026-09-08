-- no-transaction

-- Output-line retention deletes the oldest session.output.line rows first.
-- This partial index makes "the oldest expired output lines" an index range
-- read on created_at that never touches rows of other types, so the sweep
-- needs no cursor and no frontier detection.
--
-- Built concurrently so writes continue. If the build is interrupted the
-- index is left invalid and `if not exists` will not rebuild it: the sweep
-- still works but falls back to scanning the table, so check pg_index for
-- indisvalid, `drop index concurrently session_events_output_line_created_idx`
-- and rerun this statement.
create index concurrently if not exists session_events_output_line_created_idx
    on session_events (created_at)
    where event_type = 'session.output.line';
