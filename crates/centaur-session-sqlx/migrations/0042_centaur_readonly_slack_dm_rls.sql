-- Keep centaur_readonly useful for public channel context while allowing a
-- principal that carries Slack identity settings to see only its own DMs.

drop policy if exists centaur_readonly_slack_dm_sync_conversations_select
    on slack_dm_sync_conversations;
create policy centaur_readonly_slack_dm_sync_conversations_select
    on slack_dm_sync_conversations
    for select
    to centaur_readonly
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

drop policy if exists centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_dm_sync_conversation_members;
create policy centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_dm_sync_conversation_members
    for select
    to centaur_readonly
    using (
        home_team_id = centaur_current_slack_team_id()
        and user_id = centaur_current_slack_user_id()
        and is_current_member
    );

drop policy if exists centaur_readonly_slack_dm_sync_messages_select
    on slack_dm_sync_messages;
create policy centaur_readonly_slack_dm_sync_messages_select
    on slack_dm_sync_messages
    for select
    to centaur_readonly
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

drop policy if exists centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_dm_sync_message_attachments;
create policy centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_dm_sync_message_attachments
    for select
    to centaur_readonly
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

drop policy if exists centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_dm_sync_checkpoints;
create policy centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_dm_sync_checkpoints
    for select
    to centaur_readonly
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

-- Operational rows never belong in user-visible company context.
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

drop policy if exists centaur_readonly_slack_dm_context_documents_select
    on slack_dm_context_documents;
create policy centaur_readonly_slack_dm_context_documents_select
    on slack_dm_context_documents
    for select
    to centaur_readonly
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_context_documents.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_context_documents.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_dm_conversation_context_documents;
create policy centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_dm_conversation_context_documents
    for select
    to centaur_readonly
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_conversation_context_documents.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_conversation_context_documents.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );
