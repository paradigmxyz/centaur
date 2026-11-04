create table if not exists slack_sync_message_attachments (
    channel_id text not null,
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
    source_run_id text references slack_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (channel_id, message_ts, slack_file_id),
    foreign key (channel_id, message_ts)
        references slack_sync_messages(channel_id, message_ts)
        on delete cascade
);

create index if not exists idx_slack_sync_message_attachments_message
    on slack_sync_message_attachments (channel_id, message_ts);

create index if not exists idx_slack_sync_message_attachments_file
    on slack_sync_message_attachments (slack_file_id);

create index if not exists idx_slack_sync_message_attachments_status
    on slack_sync_message_attachments (download_status, updated_at desc);

grant select on slack_sync_message_attachments to centaur_slack_reader, centaur_slack_admin;

alter table slack_sync_message_attachments enable row level security;

drop policy if exists centaur_slack_message_attachments_admin_select
    on slack_sync_message_attachments;
create policy centaur_slack_message_attachments_admin_select
    on slack_sync_message_attachments
    for select
    to centaur_slack_admin
    using (true);

drop policy if exists centaur_slack_message_attachments_reader_select
    on slack_sync_message_attachments;
create policy centaur_slack_message_attachments_reader_select
    on slack_sync_message_attachments
    for select
    to centaur_slack_reader
    using (
        channel_id = nullif(current_setting('centaur.slack_channel_id', true), '')
    );
