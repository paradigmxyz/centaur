create table if not exists slack_archive_imports (
    import_id text primary key,
    workspace_id text not null,
    mode text not null default 'public_channels',
    archive_uri text not null,
    object_bucket text not null default '',
    object_key text not null default '',
    original_filename text not null default '',
    content_type text not null default '',
    file_size_bytes bigint,
    sha256 text,
    status text not null default 'upload_pending',
    workflow_run_id text,
    workflow_task_id text,
    channels_imported integer not null default 0,
    users_imported integer not null default 0,
    messages_imported integer not null default 0,
    error_text text not null default '',
    created_by text not null default '',
    uploaded_at timestamptz,
    started_at timestamptz,
    finished_at timestamptz,
    upload_url_expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    metadata jsonb not null default '{}'::jsonb,
    check (mode in ('public_channels')),
    check (status in (
        'upload_pending',
        'uploaded',
        'queued',
        'importing',
        'completed',
        'failed',
        'cancelled'
    ))
);

create index if not exists idx_slack_archive_imports_created
    on slack_archive_imports (created_at desc);

create index if not exists idx_slack_archive_imports_status
    on slack_archive_imports (status, created_at desc);

create index if not exists idx_slack_archive_imports_workspace
    on slack_archive_imports (workspace_id, created_at desc);
