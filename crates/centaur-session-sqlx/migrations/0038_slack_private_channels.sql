alter table slack_sync_channels
    add column if not exists is_private boolean not null default false;

create index if not exists idx_slack_sync_channels_private
    on slack_sync_channels (is_private, channel_id);

drop policy if exists centaur_readonly_slack_sync_channels_select
    on slack_sync_channels;
create policy centaur_readonly_slack_sync_channels_select
    on slack_sync_channels
    for select
    to centaur_readonly
    using (
        not is_private
        or channel_id = centaur_current_slack_channel_id()
    );

drop policy if exists centaur_readonly_slack_sync_message_attachments_select
    on slack_sync_message_attachments;
create policy centaur_readonly_slack_sync_message_attachments_select
    on slack_sync_message_attachments
    for select
    to centaur_readonly
    using (
        exists (
            select 1
            from slack_sync_channels channels
            where channels.channel_id = slack_sync_message_attachments.channel_id
        )
    );

drop policy if exists centaur_readonly_slack_sync_messages_select
    on slack_sync_messages;
create policy centaur_readonly_slack_sync_messages_select
    on slack_sync_messages
    for select
    to centaur_readonly
    using (
        exists (
            select 1
            from slack_sync_channels channels
            where channels.channel_id = slack_sync_messages.channel_id
        )
    );

drop policy if exists centaur_readonly_company_context_documents_select
    on company_context_documents;
create policy centaur_readonly_company_context_documents_select
    on company_context_documents
    for select
    to centaur_readonly
    using (
        source <> 'slack'
        or exists (
            select 1
            from slack_sync_channels channels
            where channels.channel_id = metadata ->> 'channel_id'
        )
    );
