create table if not exists google_drive_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    scopes_requested jsonb not null default '[]'::jsonb,
    scopes_synced jsonb not null default '[]'::jsonb,
    scopes_failed jsonb not null default '[]'::jsonb,
    files_seen integer not null default 0,
    files_upserted integer not null default 0,
    docs_fetched integer not null default 0,
    docs_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_google_drive_sync_runs_started
    on google_drive_sync_runs (started_at desc);

create table if not exists google_drive_sync_files (
    file_id text primary key,
    name text not null default '',
    mime_type text not null default '',
    web_view_link text not null default '',
    drive_id text not null default '',
    parent_ids jsonb not null default '[]'::jsonb,
    owners jsonb not null default '[]'::jsonb,
    last_modifying_user jsonb not null default '{}'::jsonb,
    trashed boolean not null default false,
    source_created_at timestamptz,
    source_modified_at timestamptz,
    text_content text not null default '',
    text_hash text not null default '',
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references google_drive_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_content_synced_at timestamptz,
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_google_drive_sync_files_modified
    on google_drive_sync_files (source_modified_at desc);

create index if not exists idx_google_drive_sync_files_text
    on google_drive_sync_files
    using gin (to_tsvector('english', coalesce(text_content, '')));

create index if not exists idx_google_drive_sync_files_parents
    on google_drive_sync_files using gin (parent_ids);

create table if not exists google_drive_sync_checkpoints (
    scope_id text primary key,
    watermark_time timestamptz,
    last_run_id text references google_drive_sync_runs(run_id) on delete set null,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
