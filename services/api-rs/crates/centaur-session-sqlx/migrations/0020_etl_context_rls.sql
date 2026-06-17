create or replace function centaur_current_slack_channel_id()
returns text
language sql
stable
as $$
    select nullif(current_setting('centaur.slack_channel_id', true), '')
$$;

create table if not exists slack_context_rls_admin_channels (
    channel_id text primary key,
    created_at timestamptz not null default now()
);

create or replace function centaur_etl_admin_channel()
returns boolean
language sql
stable
as $$
    select exists (
        select 1
        from slack_context_rls_admin_channels
        where channel_id = centaur_current_slack_channel_id()
    )
$$;

grant select on slack_context_rls_admin_channels
    to centaur_slack_reader, centaur_slack_admin;
grant execute on function centaur_current_slack_channel_id()
    to centaur_slack_reader, centaur_slack_admin;
grant execute on function centaur_etl_admin_channel()
    to centaur_slack_reader, centaur_slack_admin;

alter table slack_sync_channels enable row level security;
alter table slack_sync_users enable row level security;
alter table slack_sync_messages enable row level security;
alter table slack_sync_message_attachments enable row level security;
alter table company_context_documents enable row level security;

drop policy if exists centaur_slack_channels_admin_select on slack_sync_channels;
create policy centaur_slack_channels_admin_select
    on slack_sync_channels
    for select
    to centaur_slack_admin
    using (true);

drop policy if exists centaur_slack_channels_reader_select on slack_sync_channels;
create policy centaur_slack_channels_reader_select
    on slack_sync_channels
    for select
    to centaur_slack_reader
    using (
        centaur_etl_admin_channel()
        or channel_id = centaur_current_slack_channel_id()
    );

drop policy if exists centaur_slack_users_admin_select on slack_sync_users;
create policy centaur_slack_users_admin_select
    on slack_sync_users
    for select
    to centaur_slack_admin
    using (true);

drop policy if exists centaur_slack_users_reader_select on slack_sync_users;
create policy centaur_slack_users_reader_select
    on slack_sync_users
    for select
    to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_slack_messages_reader_select on slack_sync_messages;
create policy centaur_slack_messages_reader_select
    on slack_sync_messages
    for select
    to centaur_slack_reader
    using (
        centaur_etl_admin_channel()
        or channel_id = centaur_current_slack_channel_id()
    );

drop policy if exists centaur_slack_message_attachments_reader_select
    on slack_sync_message_attachments;
create policy centaur_slack_message_attachments_reader_select
    on slack_sync_message_attachments
    for select
    to centaur_slack_reader
    using (
        centaur_etl_admin_channel()
        or channel_id = centaur_current_slack_channel_id()
    );

drop policy if exists centaur_context_docs_reader_select on company_context_documents;
create policy centaur_context_docs_reader_select
    on company_context_documents
    for select
    to centaur_slack_reader
    using (
        centaur_etl_admin_channel()
        or (
            source = 'slack'
            and metadata ->> 'channel_id' = centaur_current_slack_channel_id()
        )
    );

grant select on
    google_drive_sync_runs,
    google_drive_sync_files,
    google_drive_sync_checkpoints,
    google_calendar_sync_runs,
    google_calendar_sync_calendars,
    google_calendar_sync_events,
    google_calendar_sync_checkpoints,
    linear_sync_runs,
    linear_sync_projects,
    linear_sync_issues,
    linear_sync_comments,
    linear_sync_checkpoints
to centaur_slack_reader, centaur_slack_admin;

alter table google_drive_sync_runs enable row level security;
alter table google_drive_sync_files enable row level security;
alter table google_drive_sync_checkpoints enable row level security;
alter table google_calendar_sync_runs enable row level security;
alter table google_calendar_sync_calendars enable row level security;
alter table google_calendar_sync_events enable row level security;
alter table google_calendar_sync_checkpoints enable row level security;
alter table linear_sync_runs enable row level security;
alter table linear_sync_projects enable row level security;
alter table linear_sync_issues enable row level security;
alter table linear_sync_comments enable row level security;
alter table linear_sync_checkpoints enable row level security;

drop policy if exists centaur_google_drive_runs_admin_select on google_drive_sync_runs;
create policy centaur_google_drive_runs_admin_select
    on google_drive_sync_runs for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_drive_runs_reader_select on google_drive_sync_runs;
create policy centaur_google_drive_runs_reader_select
    on google_drive_sync_runs for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_google_drive_files_admin_select on google_drive_sync_files;
create policy centaur_google_drive_files_admin_select
    on google_drive_sync_files for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_drive_files_reader_select on google_drive_sync_files;
create policy centaur_google_drive_files_reader_select
    on google_drive_sync_files for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_google_drive_checkpoints_admin_select
    on google_drive_sync_checkpoints;
create policy centaur_google_drive_checkpoints_admin_select
    on google_drive_sync_checkpoints for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_drive_checkpoints_reader_select
    on google_drive_sync_checkpoints;
create policy centaur_google_drive_checkpoints_reader_select
    on google_drive_sync_checkpoints for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_google_calendar_runs_admin_select on google_calendar_sync_runs;
create policy centaur_google_calendar_runs_admin_select
    on google_calendar_sync_runs for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_calendar_runs_reader_select on google_calendar_sync_runs;
create policy centaur_google_calendar_runs_reader_select
    on google_calendar_sync_runs for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_google_calendar_calendars_admin_select
    on google_calendar_sync_calendars;
create policy centaur_google_calendar_calendars_admin_select
    on google_calendar_sync_calendars for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_calendar_calendars_reader_select
    on google_calendar_sync_calendars;
create policy centaur_google_calendar_calendars_reader_select
    on google_calendar_sync_calendars for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_google_calendar_events_admin_select
    on google_calendar_sync_events;
create policy centaur_google_calendar_events_admin_select
    on google_calendar_sync_events for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_calendar_events_reader_select
    on google_calendar_sync_events;
create policy centaur_google_calendar_events_reader_select
    on google_calendar_sync_events for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_google_calendar_checkpoints_admin_select
    on google_calendar_sync_checkpoints;
create policy centaur_google_calendar_checkpoints_admin_select
    on google_calendar_sync_checkpoints for select to centaur_slack_admin using (true);
drop policy if exists centaur_google_calendar_checkpoints_reader_select
    on google_calendar_sync_checkpoints;
create policy centaur_google_calendar_checkpoints_reader_select
    on google_calendar_sync_checkpoints for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_linear_runs_admin_select on linear_sync_runs;
create policy centaur_linear_runs_admin_select
    on linear_sync_runs for select to centaur_slack_admin using (true);
drop policy if exists centaur_linear_runs_reader_select on linear_sync_runs;
create policy centaur_linear_runs_reader_select
    on linear_sync_runs for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_linear_projects_admin_select on linear_sync_projects;
create policy centaur_linear_projects_admin_select
    on linear_sync_projects for select to centaur_slack_admin using (true);
drop policy if exists centaur_linear_projects_reader_select on linear_sync_projects;
create policy centaur_linear_projects_reader_select
    on linear_sync_projects for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_linear_issues_admin_select on linear_sync_issues;
create policy centaur_linear_issues_admin_select
    on linear_sync_issues for select to centaur_slack_admin using (true);
drop policy if exists centaur_linear_issues_reader_select on linear_sync_issues;
create policy centaur_linear_issues_reader_select
    on linear_sync_issues for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_linear_comments_admin_select on linear_sync_comments;
create policy centaur_linear_comments_admin_select
    on linear_sync_comments for select to centaur_slack_admin using (true);
drop policy if exists centaur_linear_comments_reader_select on linear_sync_comments;
create policy centaur_linear_comments_reader_select
    on linear_sync_comments for select to centaur_slack_reader
    using (centaur_etl_admin_channel());

drop policy if exists centaur_linear_checkpoints_admin_select on linear_sync_checkpoints;
create policy centaur_linear_checkpoints_admin_select
    on linear_sync_checkpoints for select to centaur_slack_admin using (true);
drop policy if exists centaur_linear_checkpoints_reader_select on linear_sync_checkpoints;
create policy centaur_linear_checkpoints_reader_select
    on linear_sync_checkpoints for select to centaur_slack_reader
    using (centaur_etl_admin_channel());
