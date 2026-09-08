-- no-transaction
-- Retention sweeps only delete stdout output-line events by age.
create index concurrently if not exists session_events_stdout_created_at_idx
    on session_events (created_at)
    where event_type = 'session.output.line';
