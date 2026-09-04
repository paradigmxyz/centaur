-- no-transaction
-- Retention sweeps delete session_events by age, and the table had no index on
-- created_at -- only (thread_key, event_id) and (execution_id, event_type).
-- Without this the sweep seq-scans the largest table in the schema on every
-- pass, which is the opposite of what a background cleanup should do.
create index concurrently if not exists session_events_created_at_idx
    on session_events (created_at);
