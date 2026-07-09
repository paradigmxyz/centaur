create table if not exists attio_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    scopes_requested jsonb not null default '[]'::jsonb,
    scopes_synced jsonb not null default '[]'::jsonb,
    scopes_failed jsonb not null default '[]'::jsonb,
    meetings_seen integer not null default 0,
    meetings_upserted integer not null default 0,
    call_recordings_seen integer not null default 0,
    transcripts_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_attio_sync_runs_started
    on attio_sync_runs (started_at desc);

create table if not exists attio_sync_meetings (
    meeting_id text primary key,
    title text not null default '',
    description text not null default '',
    url text not null default '',
    linked_records jsonb not null default '[]'::jsonb,
    participants jsonb not null default '[]'::jsonb,
    organizer_id text not null default '',
    organizer_name text not null default '',
    organizer_email text not null default '',
    call_recording_ids jsonb not null default '[]'::jsonb,
    transcript_text text not null default '',
    transcript_payload jsonb not null default '[]'::jsonb,
    content_text text not null default '',
    content_hash text not null default '',
    started_at timestamptz,
    ended_at timestamptz,
    source_created_at timestamptz,
    source_updated_at timestamptz,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references attio_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_attio_sync_meetings_source_updated
    on attio_sync_meetings (source_updated_at desc);

create index if not exists idx_attio_sync_meetings_time
    on attio_sync_meetings (started_at desc);

create index if not exists idx_attio_sync_meetings_text
    on attio_sync_meetings
    using gin (to_tsvector('english', coalesce(content_text, '')));

create table if not exists attio_sync_checkpoints (
    scope_id text primary key,
    watermark_time timestamptz,
    last_run_id text references attio_sync_runs(run_id) on delete set null,
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
                'attio_sync_runs, attio_sync_meetings, attio_sync_checkpoints',
                role_name
            );
        end if;
    end loop;
end $$;

alter table attio_sync_runs enable row level security;
alter table attio_sync_meetings enable row level security;
alter table attio_sync_checkpoints enable row level security;

drop policy if exists centaur_attio_runs_admin_select on attio_sync_runs;
drop policy if exists centaur_attio_runs_reader_select on attio_sync_runs;
create policy centaur_attio_runs_reader_select
    on attio_sync_runs for select to centaur_slack_reader
    using (false);
drop policy if exists centaur_readonly_attio_sync_runs_select on attio_sync_runs;
create policy centaur_readonly_attio_sync_runs_select
    on attio_sync_runs for select to centaur_readonly using (true);

drop policy if exists centaur_attio_meetings_admin_select on attio_sync_meetings;
drop policy if exists centaur_attio_meetings_reader_select on attio_sync_meetings;
create policy centaur_attio_meetings_reader_select
    on attio_sync_meetings for select to centaur_slack_reader
    using (false);
drop policy if exists centaur_readonly_attio_sync_meetings_select
    on attio_sync_meetings;
create policy centaur_readonly_attio_sync_meetings_select
    on attio_sync_meetings for select to centaur_readonly using (true);

drop policy if exists centaur_attio_checkpoints_admin_select on attio_sync_checkpoints;
drop policy if exists centaur_attio_checkpoints_reader_select on attio_sync_checkpoints;
create policy centaur_attio_checkpoints_reader_select
    on attio_sync_checkpoints for select to centaur_slack_reader
    using (false);
drop policy if exists centaur_readonly_attio_sync_checkpoints_select
    on attio_sync_checkpoints;
create policy centaur_readonly_attio_sync_checkpoints_select
    on attio_sync_checkpoints for select to centaur_readonly using (true);

do $$
begin
    if exists (select 1 from pg_roles where rolname = 'centaur_slack_admin') then
        create policy centaur_attio_runs_admin_select
            on attio_sync_runs for select to centaur_slack_admin using (true);
        create policy centaur_attio_meetings_admin_select
            on attio_sync_meetings for select to centaur_slack_admin using (true);
        create policy centaur_attio_checkpoints_admin_select
            on attio_sync_checkpoints for select to centaur_slack_admin using (true);
    end if;
end $$;
