create table if not exists slack_dm_sync_conversations (
    home_team_id text not null,
    conversation_id text not null,
    conversation_type text not null,
    is_archived boolean not null default false,
    is_ext_shared boolean not null default false,
    raw_payload jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (home_team_id, conversation_id),
    check (home_team_id <> ''),
    check (conversation_id <> ''),
    check (conversation_type in ('im', 'mpim'))
);

create index if not exists idx_slack_dm_sync_conversations_seen
    on slack_dm_sync_conversations (last_seen_at desc);

create table if not exists slack_dm_sync_conversation_members (
    home_team_id text not null,
    conversation_id text not null,
    user_id text not null,
    user_team_id text,
    is_external boolean not null default false,
    is_current_member boolean not null default true,
    raw_payload jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (home_team_id, conversation_id, user_id),
    foreign key (home_team_id, conversation_id)
        references slack_dm_sync_conversations(home_team_id, conversation_id)
        on delete cascade,
    check (home_team_id <> ''),
    check (conversation_id <> ''),
    check (user_id <> '')
);

create index if not exists idx_slack_dm_sync_members_user
    on slack_dm_sync_conversation_members (
        home_team_id,
        user_id,
        is_current_member,
        conversation_id
    );

create table if not exists slack_dm_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    broker_credential_id text not null default '',
    source_user_id text not null default '',
    home_team_id text not null default '',
    conversations_requested integer not null default 0,
    conversations_synced integer not null default 0,
    conversations_failed integer not null default 0,
    messages_fetched integer not null default 0,
    messages_upserted integer not null default 0,
    replies_fetched integer not null default 0,
    replies_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_slack_dm_sync_runs_started
    on slack_dm_sync_runs (started_at desc);

create table if not exists slack_dm_sync_messages (
    home_team_id text not null,
    conversation_id text not null,
    message_ts text not null,
    occurred_at timestamptz,
    thread_ts text,
    parent_message_ts text,
    is_thread_root boolean not null default false,
    user_id text not null default '',
    user_team_id text,
    bot_id text not null default '',
    message_type text not null default 'message',
    message_subtype text,
    text text not null default '',
    permalink text not null default '',
    reply_count integer not null default 0,
    reply_users jsonb not null default '[]'::jsonb,
    latest_reply_ts text,
    thread_refreshed_at timestamptz,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references slack_dm_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (home_team_id, conversation_id, message_ts),
    foreign key (home_team_id, conversation_id)
        references slack_dm_sync_conversations(home_team_id, conversation_id)
        on delete cascade,
    check (home_team_id <> ''),
    check (conversation_id <> ''),
    check (message_ts <> '')
);

create index if not exists idx_slack_dm_sync_messages_thread
    on slack_dm_sync_messages (home_team_id, conversation_id, thread_ts);

create index if not exists idx_slack_dm_sync_messages_occurred
    on slack_dm_sync_messages (occurred_at desc);

create index if not exists idx_slack_dm_sync_messages_user
    on slack_dm_sync_messages (home_team_id, user_id, occurred_at desc);

create index if not exists idx_slack_dm_sync_messages_text
    on slack_dm_sync_messages
    using gin (to_tsvector('english', coalesce(text, '')));

create table if not exists slack_dm_sync_message_attachments (
    home_team_id text not null,
    conversation_id text not null,
    message_ts text not null,
    slack_file_id text not null,
    name text not null default '',
    title text not null default '',
    mimetype text not null default '',
    filetype text not null default '',
    size_bytes bigint not null default 0,
    url_private text not null default '',
    permalink text not null default '',
    download_status text not null default 'metadata_only',
    download_error text not null default '',
    content_sha256 text,
    content_bytes bytea,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references slack_dm_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (home_team_id, conversation_id, message_ts, slack_file_id),
    foreign key (home_team_id, conversation_id, message_ts)
        references slack_dm_sync_messages(home_team_id, conversation_id, message_ts)
        on delete cascade,
    check (home_team_id <> ''),
    check (conversation_id <> ''),
    check (message_ts <> ''),
    check (slack_file_id <> '')
);

create index if not exists idx_slack_dm_sync_attachments_message
    on slack_dm_sync_message_attachments (home_team_id, conversation_id, message_ts);

create index if not exists idx_slack_dm_sync_attachments_file
    on slack_dm_sync_message_attachments (slack_file_id);

create index if not exists idx_slack_dm_sync_attachments_status
    on slack_dm_sync_message_attachments (download_status, updated_at desc);

create table if not exists slack_dm_sync_checkpoints (
    broker_credential_id text not null,
    home_team_id text not null,
    conversation_id text not null,
    watermark_ts text,
    last_run_id text references slack_dm_sync_runs(run_id) on delete set null,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, home_team_id, conversation_id),
    foreign key (home_team_id, conversation_id)
        references slack_dm_sync_conversations(home_team_id, conversation_id)
        on delete cascade,
    check (broker_credential_id <> ''),
    check (home_team_id <> ''),
    check (conversation_id <> '')
);

create index if not exists idx_slack_dm_sync_checkpoints_conversation
    on slack_dm_sync_checkpoints (home_team_id, conversation_id);

create table if not exists slack_dm_sync_backfill_jobs (
    job_id bigserial primary key,
    job_key text not null unique,
    job_type text not null,
    payload_version integer not null default 1,
    broker_credential_id text not null,
    home_team_id text not null,
    conversation_id text not null,
    status text not null default 'pending',
    payload_json jsonb not null default '{}'::jsonb,
    priority integer not null default 100,
    attempt_count integer not null default 0,
    last_run_id text references slack_dm_sync_runs(run_id) on delete set null,
    last_enqueued_at timestamptz not null default now(),
    last_started_at timestamptz,
    last_completed_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    foreign key (home_team_id, conversation_id)
        references slack_dm_sync_conversations(home_team_id, conversation_id)
        on delete cascade,
    check (broker_credential_id <> ''),
    check (home_team_id <> ''),
    check (conversation_id <> '')
);

create index if not exists idx_slack_dm_sync_backfill_jobs_status_priority
    on slack_dm_sync_backfill_jobs (status, priority, updated_at);

create index if not exists idx_slack_dm_sync_backfill_jobs_conversation_status
    on slack_dm_sync_backfill_jobs (home_team_id, conversation_id, status);

create or replace function centaur_current_slack_team_id()
returns text
language sql
stable
as $$
    select nullif(current_setting('centaur.slack_team_id', true), '')
$$;

create or replace function centaur_current_slack_user_id()
returns text
language sql
stable
as $$
    select nullif(current_setting('centaur.slack_user_id', true), '')
$$;

grant execute on function centaur_current_slack_team_id()
    to centaur_slack_reader, centaur_readonly;

grant execute on function centaur_current_slack_user_id()
    to centaur_slack_reader, centaur_readonly;

grant usage on schema public to centaur_slack_reader, centaur_readonly;

grant select on
    slack_dm_sync_conversations,
    slack_dm_sync_conversation_members,
    slack_dm_sync_messages,
    slack_dm_sync_message_attachments,
    slack_dm_sync_checkpoints,
    slack_dm_sync_runs,
    slack_dm_sync_backfill_jobs
to centaur_slack_reader, centaur_readonly;

alter table slack_dm_sync_conversations enable row level security;
alter table slack_dm_sync_conversation_members enable row level security;
alter table slack_dm_sync_messages enable row level security;
alter table slack_dm_sync_message_attachments enable row level security;
alter table slack_dm_sync_checkpoints enable row level security;
alter table slack_dm_sync_runs enable row level security;
alter table slack_dm_sync_backfill_jobs enable row level security;

drop policy if exists centaur_slack_dm_conversations_reader_select
    on slack_dm_sync_conversations;
create policy centaur_slack_dm_conversations_reader_select
    on slack_dm_sync_conversations
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_sync_conversations.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_sync_conversations.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_slack_dm_members_reader_select
    on slack_dm_sync_conversation_members;
create policy centaur_slack_dm_members_reader_select
    on slack_dm_sync_conversation_members
    for select
    to centaur_slack_reader
    using (
        home_team_id = centaur_current_slack_team_id()
        and user_id = centaur_current_slack_user_id()
        and is_current_member
    );

drop policy if exists centaur_slack_dm_messages_reader_select
    on slack_dm_sync_messages;
create policy centaur_slack_dm_messages_reader_select
    on slack_dm_sync_messages
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_sync_messages.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_sync_messages.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_slack_dm_attachments_reader_select
    on slack_dm_sync_message_attachments;
create policy centaur_slack_dm_attachments_reader_select
    on slack_dm_sync_message_attachments
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_sync_message_attachments.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_sync_message_attachments.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_slack_dm_checkpoints_reader_select
    on slack_dm_sync_checkpoints;
create policy centaur_slack_dm_checkpoints_reader_select
    on slack_dm_sync_checkpoints
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_sync_checkpoints.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_sync_checkpoints.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_slack_dm_runs_reader_select
    on slack_dm_sync_runs;
create policy centaur_slack_dm_runs_reader_select
    on slack_dm_sync_runs
    for select
    to centaur_slack_reader
    using (false);

drop policy if exists centaur_slack_dm_backfill_jobs_reader_select
    on slack_dm_sync_backfill_jobs;
create policy centaur_slack_dm_backfill_jobs_reader_select
    on slack_dm_sync_backfill_jobs
    for select
    to centaur_slack_reader
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_conversations_select
    on slack_dm_sync_conversations;
create policy centaur_readonly_slack_dm_sync_conversations_select
    on slack_dm_sync_conversations
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_dm_sync_conversation_members;
create policy centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_dm_sync_conversation_members
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_messages_select
    on slack_dm_sync_messages;
create policy centaur_readonly_slack_dm_sync_messages_select
    on slack_dm_sync_messages
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_dm_sync_message_attachments;
create policy centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_dm_sync_message_attachments
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_dm_sync_checkpoints;
create policy centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_dm_sync_checkpoints
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_runs_select
    on slack_dm_sync_runs;
create policy centaur_readonly_slack_dm_sync_runs_select
    on slack_dm_sync_runs
    for select
    to centaur_readonly
    using (false);

drop policy if exists centaur_readonly_slack_dm_sync_backfill_jobs_select
    on slack_dm_sync_backfill_jobs;
create policy centaur_readonly_slack_dm_sync_backfill_jobs_select
    on slack_dm_sync_backfill_jobs
    for select
    to centaur_readonly
    using (false);
