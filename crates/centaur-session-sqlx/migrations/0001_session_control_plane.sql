create table if not exists sessions (
    thread_key text primary key,
    sandbox_id text,
    harness_type text not null,
    harness_thread_id text,
    status text not null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint sessions_thread_key_len check (octet_length(thread_key) <= 512),
    constraint sessions_thread_key_namespaced check (position(':' in thread_key) > 1),
    constraint sessions_harness_type_len check (octet_length(harness_type) between 1 and 64),
    constraint sessions_harness_type_supported check (harness_type in ('codex', 'amp', 'claudecode'))
);

create table if not exists session_messages (
    message_id text primary key,
    thread_key text not null references sessions(thread_key) on delete cascade,
    role text not null,
    parts jsonb not null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists session_messages_thread_created_idx
    on session_messages (thread_key, created_at, message_id);

create table if not exists session_executions (
    execution_id text primary key,
    thread_key text not null references sessions(thread_key) on delete cascade,
    status text not null,
    metadata jsonb not null default '{}'::jsonb,
    error text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    started_at timestamptz,
    completed_at timestamptz
);

create index if not exists session_executions_thread_created_idx
    on session_executions (thread_key, created_at, execution_id);

create unique index if not exists session_executions_one_active_idx
    on session_executions (thread_key)
    where status in ('queued', 'running');

create table if not exists session_events (
    event_id bigint generated always as identity primary key,
    thread_key text not null references sessions(thread_key) on delete cascade,
    execution_id text references session_executions(execution_id) on delete set null,
    event_type text not null,
    payload jsonb not null,
    created_at timestamptz not null default now()
);

create index if not exists session_events_thread_event_idx
    on session_events (thread_key, event_id);
