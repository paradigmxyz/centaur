create or replace function centaur_current_slack_channel_id()
returns text
language sql
stable
as $$
    select nullif(current_setting('centaur.slack_channel_id', true), '')
$$;

drop policy if exists centaur_slack_channels_admin_select on slack_sync_channels;
drop policy if exists centaur_slack_channels_reader_select on slack_sync_channels;
create policy centaur_slack_channels_reader_select
    on slack_sync_channels
    for select
    to centaur_slack_reader
    using (channel_id = centaur_current_slack_channel_id());

drop policy if exists centaur_slack_users_admin_select on slack_sync_users;
drop policy if exists centaur_slack_users_reader_select on slack_sync_users;
create policy centaur_slack_users_reader_select
    on slack_sync_users
    for select
    to centaur_slack_reader
    using (false);

drop policy if exists centaur_slack_messages_admin_select on slack_sync_messages;
drop policy if exists centaur_slack_messages_reader_select on slack_sync_messages;
create policy centaur_slack_messages_reader_select
    on slack_sync_messages
    for select
    to centaur_slack_reader
    using (channel_id = centaur_current_slack_channel_id());

drop policy if exists centaur_slack_message_attachments_admin_select
    on slack_sync_message_attachments;
drop policy if exists centaur_slack_message_attachments_reader_select
    on slack_sync_message_attachments;
create policy centaur_slack_message_attachments_reader_select
    on slack_sync_message_attachments
    for select
    to centaur_slack_reader
    using (channel_id = centaur_current_slack_channel_id());

drop policy if exists centaur_context_docs_admin_select on company_context_documents;
drop policy if exists centaur_context_docs_reader_select on company_context_documents;
create policy centaur_context_docs_reader_select
    on company_context_documents
    for select
    to centaur_slack_reader
    using (
        source = 'slack'
        and metadata ->> 'channel_id' = centaur_current_slack_channel_id()
    );

drop policy if exists centaur_google_drive_runs_admin_select on google_drive_sync_runs;
drop policy if exists centaur_google_drive_runs_reader_select on google_drive_sync_runs;
create policy centaur_google_drive_runs_reader_select
    on google_drive_sync_runs for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_google_drive_files_admin_select on google_drive_sync_files;
drop policy if exists centaur_google_drive_files_reader_select on google_drive_sync_files;
create policy centaur_google_drive_files_reader_select
    on google_drive_sync_files for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_google_drive_checkpoints_admin_select
    on google_drive_sync_checkpoints;
drop policy if exists centaur_google_drive_checkpoints_reader_select
    on google_drive_sync_checkpoints;
create policy centaur_google_drive_checkpoints_reader_select
    on google_drive_sync_checkpoints for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_google_calendar_runs_admin_select on google_calendar_sync_runs;
drop policy if exists centaur_google_calendar_runs_reader_select on google_calendar_sync_runs;
create policy centaur_google_calendar_runs_reader_select
    on google_calendar_sync_runs for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_google_calendar_calendars_admin_select
    on google_calendar_sync_calendars;
drop policy if exists centaur_google_calendar_calendars_reader_select
    on google_calendar_sync_calendars;
create policy centaur_google_calendar_calendars_reader_select
    on google_calendar_sync_calendars for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_google_calendar_events_admin_select
    on google_calendar_sync_events;
drop policy if exists centaur_google_calendar_events_reader_select
    on google_calendar_sync_events;
create policy centaur_google_calendar_events_reader_select
    on google_calendar_sync_events for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_google_calendar_checkpoints_admin_select
    on google_calendar_sync_checkpoints;
drop policy if exists centaur_google_calendar_checkpoints_reader_select
    on google_calendar_sync_checkpoints;
create policy centaur_google_calendar_checkpoints_reader_select
    on google_calendar_sync_checkpoints for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_linear_runs_admin_select on linear_sync_runs;
drop policy if exists centaur_linear_runs_reader_select on linear_sync_runs;
create policy centaur_linear_runs_reader_select
    on linear_sync_runs for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_linear_projects_admin_select on linear_sync_projects;
drop policy if exists centaur_linear_projects_reader_select on linear_sync_projects;
create policy centaur_linear_projects_reader_select
    on linear_sync_projects for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_linear_issues_admin_select on linear_sync_issues;
drop policy if exists centaur_linear_issues_reader_select on linear_sync_issues;
create policy centaur_linear_issues_reader_select
    on linear_sync_issues for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_linear_comments_admin_select on linear_sync_comments;
drop policy if exists centaur_linear_comments_reader_select on linear_sync_comments;
create policy centaur_linear_comments_reader_select
    on linear_sync_comments for select to centaur_slack_reader
    using (false);

drop policy if exists centaur_linear_checkpoints_admin_select on linear_sync_checkpoints;
drop policy if exists centaur_linear_checkpoints_reader_select on linear_sync_checkpoints;
create policy centaur_linear_checkpoints_reader_select
    on linear_sync_checkpoints for select to centaur_slack_reader
    using (false);

revoke select on
    slack_sync_channels,
    slack_sync_users,
    slack_sync_messages,
    slack_sync_message_attachments,
    company_context_documents,
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
from centaur_slack_admin;

revoke usage on schema public from centaur_slack_admin;
revoke execute on function centaur_current_slack_channel_id() from centaur_slack_admin;

drop function if exists centaur_etl_admin_channel();
drop table if exists slack_context_rls_admin_channels;
drop role if exists centaur_slack_admin;
