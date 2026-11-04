create table if not exists user_feedback (
    feedback_id text primary key,
    source text not null,
    message text not null,
    user_id text,
    channel_id text,
    thread_ts text,
    execution_id text references session_executions(execution_id) on delete set null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    constraint user_feedback_source_len check (octet_length(source) between 1 and 64),
    constraint user_feedback_message_len check (octet_length(message) between 1 and 20000)
);

create index if not exists user_feedback_created_idx
    on user_feedback (created_at desc, feedback_id);

create index if not exists user_feedback_execution_idx
    on user_feedback (execution_id)
    where execution_id is not null;
