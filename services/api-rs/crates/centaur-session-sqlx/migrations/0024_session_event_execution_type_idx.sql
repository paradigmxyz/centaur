create index if not exists session_events_execution_type_idx
    on session_events (execution_id, event_type)
    where execution_id is not null;
