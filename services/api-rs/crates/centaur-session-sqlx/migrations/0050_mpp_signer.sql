create table if not exists mpp_registry_cache (
    singleton boolean primary key default true check (singleton),
    schema_version smallint not null default 1 check (schema_version = 1),
    catalog jsonb not null,
    fetched_at timestamptz not null,
    etag text,
    last_modified text,
    updated_at timestamptz not null default now()
);

create table if not exists mpp_charge_attempts (
    attempt_id uuid primary key,
    challenge_hash text not null unique,
    service_id text not null,
    method text not null,
    path_template text not null,
    amount_atomic bigint not null check (amount_atomic >= 0),
    currency text not null,
    sandbox_id text not null,
    execution_id text not null references session_executions(execution_id) on delete restrict,
    budget_reserved boolean not null default false,
    status text not null check (
        status in (
            'reserving',
            'authorized',
            'sign_failed',
            'settled',
            'released',
            'unknown'
        )
    ),
    error_code text,
    receipt_hash text,
    replay_status integer,
    created_at timestamptz not null default now(),
    authorized_at timestamptz,
    completed_at timestamptz
);

create index if not exists mpp_charge_attempts_created_idx
    on mpp_charge_attempts (created_at);

create index if not exists mpp_charge_attempts_budget_idx
    on mpp_charge_attempts (created_at, status)
    where status in ('reserving', 'authorized', 'settled', 'unknown');
