create table if not exists session_warm_sandboxes (
    sandbox_id text primary key,
    workload_key text not null,
    status text not null,
    claimed_thread_key text references sessions(thread_key) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    claimed_at timestamptz,
    last_error text,
    constraint session_warm_sandboxes_status_supported
        check (status in ('ready', 'claimed', 'failed'))
);

create index if not exists session_warm_sandboxes_ready_idx
    on session_warm_sandboxes (workload_key, status, created_at)
    where status = 'ready';

create index if not exists session_warm_sandboxes_claimed_thread_idx
    on session_warm_sandboxes (claimed_thread_key, claimed_at)
    where claimed_thread_key is not null;
