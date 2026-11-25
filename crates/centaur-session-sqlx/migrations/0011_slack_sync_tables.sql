create table if not exists slack_sync_channels (
    channel_id text primary key,
    channel_name text not null default '',
    is_archived boolean not null default false,
    is_syncable boolean not null default false,
    topic text not null default '',
    purpose text not null default '',
    member_count integer not null default 0,
    raw_payload jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

alter table slack_sync_channels
    add column if not exists is_syncable boolean not null default false;

create index if not exists idx_slack_sync_channels_syncable
    on slack_sync_channels (is_syncable, channel_name);

create table if not exists slack_sync_users (
    user_id text primary key,
    user_name text not null default '',
    real_name text not null default '',
    display_name text not null default '',
    is_bot boolean not null default false,
    is_deleted boolean not null default false,
    team_id text not null default '',
    raw_payload jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_slack_sync_users_real_name
    on slack_sync_users (real_name);

create table if not exists slack_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    channels_requested jsonb not null default '[]'::jsonb,
    channels_synced jsonb not null default '[]'::jsonb,
    channels_skipped jsonb not null default '[]'::jsonb,
    channels_failed jsonb not null default '[]'::jsonb,
    messages_fetched integer not null default 0,
    messages_upserted integer not null default 0,
    threads_fetched integer not null default 0,
    replies_fetched integer not null default 0,
    replies_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_slack_sync_runs_started
    on slack_sync_runs (started_at desc);

create table if not exists slack_sync_messages (
    channel_id text not null references slack_sync_channels(channel_id) on delete cascade,
    message_ts text not null,
    occurred_at timestamptz,
    thread_ts text,
    parent_message_ts text,
    is_thread_root boolean not null default false,
    user_id text not null default '',
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
    source_run_id text references slack_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (channel_id, message_ts)
);

alter table slack_sync_messages
    add column if not exists thread_refreshed_at timestamptz;

create index if not exists idx_slack_sync_messages_thread
    on slack_sync_messages (channel_id, thread_ts);

create index if not exists idx_slack_sync_messages_occurred
    on slack_sync_messages (occurred_at desc);

create index if not exists idx_slack_sync_messages_user
    on slack_sync_messages (user_id, occurred_at desc);

create index if not exists idx_slack_sync_messages_text
    on slack_sync_messages
    using gin (to_tsvector('english', coalesce(text, '')));

create table if not exists slack_sync_checkpoints (
    channel_id text primary key references slack_sync_channels(channel_id) on delete cascade,
    watermark_ts text,
    last_run_id text references slack_sync_runs(run_id) on delete set null,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists slack_sync_backfill_jobs (
    job_id bigserial primary key,
    job_key text not null unique,
    job_type text not null,
    payload_version integer not null default 1,
    channel_id text not null references slack_sync_channels(channel_id) on delete cascade,
    status text not null default 'pending',
    payload_json jsonb not null default '{}'::jsonb,
    priority integer not null default 100,
    attempt_count integer not null default 0,
    last_run_id text references slack_sync_runs(run_id) on delete set null,
    last_enqueued_at timestamptz not null default now(),
    last_started_at timestamptz,
    last_completed_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_slack_sync_backfill_jobs_status_priority
    on slack_sync_backfill_jobs (status, priority, updated_at);

create index if not exists idx_slack_sync_backfill_jobs_channel_status
    on slack_sync_backfill_jobs (channel_id, status);

create unique index if not exists uq_slack_sync_channel_bootstrap_backfill
    on slack_sync_backfill_jobs (channel_id)
    where job_type = 'channel_bootstrap';
