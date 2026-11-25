create extension if not exists pg_search;

create table if not exists google_docs_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    broker_credential_id text not null default '',
    provider_subject text not null default '',
    provider_email text not null default '',
    files_seen integer not null default 0,
    files_upserted integer not null default 0,
    docs_fetched integer not null default 0,
    docs_upserted integer not null default 0,
    chunks_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    check (broker_credential_id <> '')
);

create index if not exists idx_google_docs_sync_runs_started
    on google_docs_sync_runs (started_at desc);

create index if not exists idx_google_docs_sync_runs_credential_started
    on google_docs_sync_runs (broker_credential_id, started_at desc);

create index if not exists idx_google_docs_sync_runs_subject_started
    on google_docs_sync_runs (provider_subject, started_at desc)
    where provider_subject <> '';

create table if not exists google_docs_sync_files (
    file_id text primary key,
    drive_id text not null default '',
    name text not null default '',
    mime_type text not null default '',
    web_view_link text not null default '',
    owners jsonb not null default '[]'::jsonb,
    last_modifying_user jsonb not null default '{}'::jsonb,
    capabilities jsonb not null default '{}'::jsonb,
    labels jsonb not null default '{}'::jsonb,
    trashed boolean not null default false,
    explicitly_trashed boolean not null default false,
    source_created_at timestamptz,
    source_modified_at timestamptz,
    source_version text not null default '',
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references google_docs_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (file_id <> '')
);

create index if not exists idx_google_docs_sync_files_modified
    on google_docs_sync_files (source_modified_at desc);

create index if not exists idx_google_docs_sync_files_drive
    on google_docs_sync_files (drive_id, source_modified_at desc)
    where drive_id <> '';

create index if not exists idx_google_docs_sync_files_name
    on google_docs_sync_files (name);

create index if not exists idx_google_docs_sync_files_raw_payload
    on google_docs_sync_files using gin (raw_payload);

create table if not exists google_docs_sync_file_observations (
    broker_credential_id text not null,
    observed_file_id text not null,
    file_id text not null references google_docs_sync_files(file_id) on delete cascade,
    provider_subject text not null default '',
    provider_email text not null default '',
    observed_name text not null default '',
    observed_mime_type text not null default '',
    observed_web_view_link text not null default '',
    shortcut_target_file_id text not null default '',
    role_hint text not null default '',
    permission_ids jsonb not null default '[]'::jsonb,
    active boolean not null default true,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references google_docs_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, observed_file_id),
    check (broker_credential_id <> ''),
    check (observed_file_id <> ''),
    check (file_id <> '')
);

create index if not exists idx_google_docs_sync_observations_file_subject
    on google_docs_sync_file_observations (file_id, provider_subject)
    where active and provider_subject <> '';

create index if not exists idx_google_docs_sync_observations_subject_seen
    on google_docs_sync_file_observations (provider_subject, last_seen_at desc)
    where active and provider_subject <> '';

create index if not exists idx_google_docs_sync_observations_credential_seen
    on google_docs_sync_file_observations (broker_credential_id, last_seen_at desc);

create index if not exists idx_google_docs_sync_observations_raw_payload
    on google_docs_sync_file_observations using gin (raw_payload);

create table if not exists google_docs_sync_document_contents (
    file_id text primary key references google_docs_sync_files(file_id) on delete cascade,
    title text not null default '',
    text_content text not null default '',
    text_hash text not null default '',
    export_mime_type text not null default '',
    exported_at timestamptz,
    source_modified_at timestamptz,
    source_version text not null default '',
    source_run_id text references google_docs_sync_runs(run_id) on delete set null,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_google_docs_sync_document_contents_modified
    on google_docs_sync_document_contents (source_modified_at desc);

create index if not exists idx_google_docs_sync_document_contents_text
    on google_docs_sync_document_contents
    using gin (to_tsvector('english', coalesce(text_content, '')));

create table if not exists google_docs_sync_checkpoints (
    broker_credential_id text primary key,
    provider_subject text not null default '',
    provider_email text not null default '',
    start_page_token text not null default '',
    changes_page_token text not null default '',
    last_full_sync_at timestamptz,
    last_incremental_sync_at timestamptz,
    last_run_id text references google_docs_sync_runs(run_id) on delete set null,
    last_error text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (broker_credential_id <> '')
);

create index if not exists idx_google_docs_sync_checkpoints_subject
    on google_docs_sync_checkpoints (provider_subject)
    where provider_subject <> '';

create table if not exists google_docs_context_documents (
    document_id text primary key,
    file_id text not null references google_docs_sync_files(file_id) on delete cascade,
    chunk_id text not null default '',
    title text not null default '',
    body text not null default '',
    url text not null default '',
    provider_author_id text not null default '',
    provider_author_name text not null default '',
    mime_type text not null default '',
    drive_id text not null default '',
    source_created_at timestamptz,
    source_modified_at timestamptz,
    source_version text not null default '',
    content_hash text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (file_id, chunk_id),
    check (document_id <> ''),
    check (file_id <> '')
);

create index if not exists idx_google_docs_context_documents_file
    on google_docs_context_documents (file_id);

create index if not exists idx_google_docs_context_documents_modified
    on google_docs_context_documents (source_modified_at desc);

create index if not exists idx_google_docs_context_documents_drive_modified
    on google_docs_context_documents (drive_id, source_modified_at desc)
    where drive_id <> '';

create index if not exists idx_google_docs_context_documents_metadata
    on google_docs_context_documents using gin (metadata);

drop index if exists idx_google_docs_context_documents_bm25;

create index idx_google_docs_context_documents_bm25
    on google_docs_context_documents
    using bm25 (
        document_id,
        title,
        body,
        file_id,
        chunk_id,
        url,
        provider_author_id,
        provider_author_name,
        mime_type,
        drive_id,
        source_created_at,
        source_modified_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "file_id": {
                "tokenizer": {"type": "keyword"}
            },
            "chunk_id": {
                "tokenizer": {"type": "keyword"}
            },
            "provider_author_id": {
                "tokenizer": {"type": "keyword"}
            },
            "drive_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create or replace function centaur_current_google_subject()
returns text
language sql
stable
as $$
    select nullif(current_setting('centaur.google_subject', true), '')
$$;

create or replace function centaur_current_google_email()
returns text
language sql
stable
as $$
    select nullif(current_setting('centaur.google_email', true), '')
$$;

grant usage on schema public
    to centaur_slack_reader, centaur_readonly;

grant execute on function centaur_current_google_subject()
    to centaur_slack_reader, centaur_readonly;

grant execute on function centaur_current_google_email()
    to centaur_slack_reader, centaur_readonly;

grant select on
    google_docs_sync_runs,
    google_docs_sync_files,
    google_docs_sync_file_observations,
    google_docs_sync_document_contents,
    google_docs_sync_checkpoints,
    google_docs_context_documents
to centaur_slack_reader, centaur_readonly;

alter table google_docs_sync_runs enable row level security;
alter table google_docs_sync_files enable row level security;
alter table google_docs_sync_file_observations enable row level security;
alter table google_docs_sync_document_contents enable row level security;
alter table google_docs_sync_checkpoints enable row level security;
alter table google_docs_context_documents enable row level security;

drop policy if exists centaur_google_docs_runs_reader_select
    on google_docs_sync_runs;
create policy centaur_google_docs_runs_reader_select
    on google_docs_sync_runs
    for select
    to centaur_slack_reader
    using (
        provider_subject <> ''
        and provider_subject = centaur_current_google_subject()
    );

drop policy if exists centaur_google_docs_files_reader_select
    on google_docs_sync_files;
create policy centaur_google_docs_files_reader_select
    on google_docs_sync_files
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from google_docs_sync_file_observations observations
            where observations.file_id = google_docs_sync_files.file_id
              and observations.provider_subject = centaur_current_google_subject()
              and observations.active
        )
    );

drop policy if exists centaur_google_docs_observations_reader_select
    on google_docs_sync_file_observations;
create policy centaur_google_docs_observations_reader_select
    on google_docs_sync_file_observations
    for select
    to centaur_slack_reader
    using (
        provider_subject <> ''
        and provider_subject = centaur_current_google_subject()
        and active
    );

drop policy if exists centaur_google_docs_contents_reader_select
    on google_docs_sync_document_contents;
create policy centaur_google_docs_contents_reader_select
    on google_docs_sync_document_contents
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from google_docs_sync_file_observations observations
            where observations.file_id = google_docs_sync_document_contents.file_id
              and observations.provider_subject = centaur_current_google_subject()
              and observations.active
        )
    );

drop policy if exists centaur_google_docs_checkpoints_reader_select
    on google_docs_sync_checkpoints;
create policy centaur_google_docs_checkpoints_reader_select
    on google_docs_sync_checkpoints
    for select
    to centaur_slack_reader
    using (
        provider_subject <> ''
        and provider_subject = centaur_current_google_subject()
    );

drop policy if exists centaur_google_docs_context_documents_reader_select
    on google_docs_context_documents;
create policy centaur_google_docs_context_documents_reader_select
    on google_docs_context_documents
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from google_docs_sync_file_observations observations
            where observations.file_id = google_docs_context_documents.file_id
              and observations.provider_subject = centaur_current_google_subject()
              and observations.active
        )
    );

drop policy if exists centaur_readonly_google_docs_sync_runs_select
    on google_docs_sync_runs;
create policy centaur_readonly_google_docs_sync_runs_select
    on google_docs_sync_runs
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_google_docs_sync_files_select
    on google_docs_sync_files;
create policy centaur_readonly_google_docs_sync_files_select
    on google_docs_sync_files
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_google_docs_sync_file_observations_select
    on google_docs_sync_file_observations;
create policy centaur_readonly_google_docs_sync_file_observations_select
    on google_docs_sync_file_observations
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_google_docs_sync_document_contents_select
    on google_docs_sync_document_contents;
create policy centaur_readonly_google_docs_sync_document_contents_select
    on google_docs_sync_document_contents
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_google_docs_sync_checkpoints_select
    on google_docs_sync_checkpoints;
create policy centaur_readonly_google_docs_sync_checkpoints_select
    on google_docs_sync_checkpoints
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_google_docs_context_documents_select
    on google_docs_context_documents;
create policy centaur_readonly_google_docs_context_documents_select
    on google_docs_context_documents
    for select
    to centaur_readonly
    using (false);
