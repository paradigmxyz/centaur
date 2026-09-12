create table if not exists session_sandbox_leases (
    sandbox_id text primary key,
    owner_id   text not null,
    expires_at timestamptz not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists session_sandbox_leases_expires_at_idx
    on session_sandbox_leases (expires_at);
