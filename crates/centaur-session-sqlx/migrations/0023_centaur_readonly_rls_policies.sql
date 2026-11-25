drop policy if exists centaur_readonly_company_context_documents_select
    on company_context_documents;
create policy centaur_readonly_company_context_documents_select
    on company_context_documents
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_calendar_sync_calendars_select
    on google_calendar_sync_calendars;
create policy centaur_readonly_google_calendar_sync_calendars_select
    on google_calendar_sync_calendars
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_calendar_sync_checkpoints_select
    on google_calendar_sync_checkpoints;
create policy centaur_readonly_google_calendar_sync_checkpoints_select
    on google_calendar_sync_checkpoints
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_calendar_sync_events_select
    on google_calendar_sync_events;
create policy centaur_readonly_google_calendar_sync_events_select
    on google_calendar_sync_events
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_calendar_sync_runs_select
    on google_calendar_sync_runs;
create policy centaur_readonly_google_calendar_sync_runs_select
    on google_calendar_sync_runs
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_drive_sync_checkpoints_select
    on google_drive_sync_checkpoints;
create policy centaur_readonly_google_drive_sync_checkpoints_select
    on google_drive_sync_checkpoints
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_drive_sync_files_select
    on google_drive_sync_files;
create policy centaur_readonly_google_drive_sync_files_select
    on google_drive_sync_files
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_google_drive_sync_runs_select
    on google_drive_sync_runs;
create policy centaur_readonly_google_drive_sync_runs_select
    on google_drive_sync_runs
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_linear_sync_checkpoints_select
    on linear_sync_checkpoints;
create policy centaur_readonly_linear_sync_checkpoints_select
    on linear_sync_checkpoints
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_linear_sync_comments_select
    on linear_sync_comments;
create policy centaur_readonly_linear_sync_comments_select
    on linear_sync_comments
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_linear_sync_issues_select
    on linear_sync_issues;
create policy centaur_readonly_linear_sync_issues_select
    on linear_sync_issues
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_linear_sync_projects_select
    on linear_sync_projects;
create policy centaur_readonly_linear_sync_projects_select
    on linear_sync_projects
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_linear_sync_runs_select
    on linear_sync_runs;
create policy centaur_readonly_linear_sync_runs_select
    on linear_sync_runs
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_slack_sync_channels_select
    on slack_sync_channels;
create policy centaur_readonly_slack_sync_channels_select
    on slack_sync_channels
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_slack_sync_message_attachments_select
    on slack_sync_message_attachments;
create policy centaur_readonly_slack_sync_message_attachments_select
    on slack_sync_message_attachments
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_slack_sync_messages_select
    on slack_sync_messages;
create policy centaur_readonly_slack_sync_messages_select
    on slack_sync_messages
    for select
    to centaur_readonly
    using (true);

drop policy if exists centaur_readonly_slack_sync_users_select
    on slack_sync_users;
create policy centaur_readonly_slack_sync_users_select
    on slack_sync_users
    for select
    to centaur_readonly
    using (true);
