create extension if not exists pg_search;

create table if not exists granola_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    scopes_requested jsonb not null default '[]'::jsonb,
    scopes_synced jsonb not null default '[]'::jsonb,
    scopes_failed jsonb not null default '[]'::jsonb,
    notes_seen integer not null default 0,
    notes_upserted integer not null default 0,
    transcripts_seen integer not null default 0,
    transcripts_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_granola_sync_runs_started
    on granola_sync_runs (started_at desc);

create table if not exists granola_sync_notes (
    note_id text primary key,
    title text not null default '',
    owner_id text not null default '',
    owner_email text not null default '',
    owner_name text not null default '',
    attendees jsonb not null default '[]'::jsonb,
    access_emails text[] not null default array[]::text[],
    calendar_event jsonb not null default '{}'::jsonb,
    summary_markdown text not null default '',
    summary_text text not null default '',
    transcript_text text not null default '',
    transcript_payload jsonb not null default '[]'::jsonb,
    url text not null default '',
    content_text text not null default '',
    content_hash text not null default '',
    source_created_at timestamptz,
    source_updated_at timestamptz,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references granola_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_granola_sync_notes_source_updated
    on granola_sync_notes (source_updated_at desc);

create index if not exists idx_granola_sync_notes_owner
    on granola_sync_notes (owner_email, source_created_at desc);

create index if not exists idx_granola_sync_notes_access_emails
    on granola_sync_notes using gin (access_emails);

create index if not exists idx_granola_sync_notes_text
    on granola_sync_notes
    using gin (to_tsvector('english', coalesce(content_text, '')));

create table if not exists granola_context_documents (
    document_id text primary key,
    note_id text not null references granola_sync_notes(note_id) on delete cascade,
    title text not null default '',
    body text not null default '',
    url text not null default '',
    owner_id text not null default '',
    owner_email text not null default '',
    owner_name text not null default '',
    access_emails text[] not null default array[]::text[],
    attendee_labels text[] not null default array[]::text[],
    occurred_at timestamptz,
    source_updated_at timestamptz,
    content_hash text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (note_id),
    check (document_id <> ''),
    check (note_id <> '')
);

create index if not exists idx_granola_context_documents_note_time
    on granola_context_documents (note_id, occurred_at desc);

create index if not exists idx_granola_context_documents_owner_time
    on granola_context_documents (owner_email, occurred_at desc);

create index if not exists idx_granola_context_documents_access_emails
    on granola_context_documents using gin (access_emails);

create index if not exists idx_granola_context_documents_metadata
    on granola_context_documents using gin (metadata);

drop index if exists idx_granola_context_documents_bm25;

create index idx_granola_context_documents_bm25
    on granola_context_documents
    using bm25 (
        document_id,
        note_id,
        title,
        body,
        url,
        owner_id,
        owner_email,
        owner_name,
        occurred_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "note_id": {
                "tokenizer": {"type": "keyword"}
            },
            "owner_id": {
                "tokenizer": {"type": "keyword"}
            },
            "owner_email": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create table if not exists granola_sync_checkpoints (
    scope_id text primary key,
    watermark_time timestamptz,
    last_run_id text references granola_sync_runs(run_id) on delete set null,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

do $$
declare
    role_name text;
begin
    foreach role_name in array array[
        'centaur_slack_reader',
        'centaur_slack_admin',
        'centaur_readonly'
    ] loop
        if exists (select 1 from pg_roles where rolname = role_name) then
            execute format(
                'grant select on %s to %I',
                'granola_sync_runs, granola_sync_notes, granola_context_documents, granola_sync_checkpoints',
                role_name
            );
        end if;
    end loop;
end $$;

alter table granola_sync_runs enable row level security;
alter table granola_sync_notes enable row level security;
alter table granola_context_documents enable row level security;
alter table granola_sync_checkpoints enable row level security;

create or replace function centaur_current_slack_user_email()
returns text
language sql
stable
security definer
set search_path = public
as $$
    select coalesce(
        lower(nullif(current_setting('centaur.user_email', true), '')),
        (
            select lower(nullif(coalesce(
                users.raw_payload #>> '{profile,email}',
                users.raw_payload ->> 'email'
            ), ''))
            from slack_sync_users users
            where users.team_id = centaur_current_slack_team_id()
              and users.user_id = centaur_current_slack_user_id()
            limit 1
        )
    )
$$;

create or replace function centaur_granola_current_user_can_read(
    p_access_emails text[]
)
returns boolean
language sql
stable
as $$
    select coalesce(
        centaur_current_slack_user_email() = any(coalesce(p_access_emails, array[]::text[])),
        false
    )
$$;

do $$
declare
    role_name text;
begin
    foreach role_name in array array[
        'centaur_slack_reader',
        'centaur_slack_admin',
        'centaur_readonly'
    ] loop
        if exists (select 1 from pg_roles where rolname = role_name) then
            execute format(
                'grant execute on function centaur_current_slack_user_email() to %I',
                role_name
            );
            execute format(
                'grant execute on function centaur_granola_current_user_can_read(text[]) to %I',
                role_name
            );
        end if;
    end loop;
end $$;

drop policy if exists centaur_granola_runs_admin_select on granola_sync_runs;
drop policy if exists centaur_granola_runs_reader_select on granola_sync_runs;
create policy centaur_granola_runs_reader_select
    on granola_sync_runs for select to centaur_slack_reader
    using (false);
drop policy if exists centaur_readonly_granola_sync_runs_select on granola_sync_runs;
create policy centaur_readonly_granola_sync_runs_select
    on granola_sync_runs for select to centaur_readonly using (false);

drop policy if exists centaur_granola_notes_admin_select on granola_sync_notes;
drop policy if exists centaur_granola_notes_reader_select on granola_sync_notes;
create policy centaur_granola_notes_reader_select
    on granola_sync_notes for select to centaur_slack_reader
    using (centaur_granola_current_user_can_read(access_emails));
drop policy if exists centaur_readonly_granola_sync_notes_select on granola_sync_notes;
create policy centaur_readonly_granola_sync_notes_select
    on granola_sync_notes for select to centaur_readonly using (false);

drop policy if exists centaur_granola_context_documents_admin_select
    on granola_context_documents;
drop policy if exists centaur_granola_context_documents_reader_select
    on granola_context_documents;
create policy centaur_granola_context_documents_reader_select
    on granola_context_documents for select to centaur_slack_reader
    using (centaur_granola_current_user_can_read(access_emails));
drop policy if exists centaur_readonly_granola_context_documents_select
    on granola_context_documents;
create policy centaur_readonly_granola_context_documents_select
    on granola_context_documents for select to centaur_readonly using (false);

drop policy if exists centaur_granola_checkpoints_admin_select
    on granola_sync_checkpoints;
drop policy if exists centaur_granola_checkpoints_reader_select
    on granola_sync_checkpoints;
create policy centaur_granola_checkpoints_reader_select
    on granola_sync_checkpoints for select to centaur_slack_reader
    using (false);
drop policy if exists centaur_readonly_granola_sync_checkpoints_select
    on granola_sync_checkpoints;
create policy centaur_readonly_granola_sync_checkpoints_select
    on granola_sync_checkpoints for select to centaur_readonly using (false);

do $$
begin
    if exists (select 1 from pg_roles where rolname = 'centaur_slack_admin') then
        create policy centaur_granola_runs_admin_select
            on granola_sync_runs for select to centaur_slack_admin using (true);
        create policy centaur_granola_notes_admin_select
            on granola_sync_notes for select to centaur_slack_admin using (true);
        create policy centaur_granola_context_documents_admin_select
            on granola_context_documents for select to centaur_slack_admin using (true);
        create policy centaur_granola_checkpoints_admin_select
            on granola_sync_checkpoints for select to centaur_slack_admin using (true);
    end if;
end $$;
