create table if not exists slack_context_rls_admin_channels (
    channel_id text primary key,
    created_at timestamptz not null default now()
);

grant select on slack_context_rls_admin_channels to centaur_slack_reader, centaur_slack_admin;

drop policy if exists centaur_slack_messages_reader_select on slack_sync_messages;
create policy centaur_slack_messages_reader_select
    on slack_sync_messages
    for select
    to centaur_slack_reader
    using (
        channel_id = nullif(current_setting('centaur.slack_channel_id', true), '')
        or exists (
            select 1
            from slack_context_rls_admin_channels admins
            where admins.channel_id = nullif(current_setting('centaur.slack_channel_id', true), '')
        )
    );

drop policy if exists centaur_context_docs_reader_select on company_context_documents;
create policy centaur_context_docs_reader_select
    on company_context_documents
    for select
    to centaur_slack_reader
    using (
        source <> 'slack'
        or metadata ->> 'channel_id' = nullif(current_setting('centaur.slack_channel_id', true), '')
        or exists (
            select 1
            from slack_context_rls_admin_channels admins
            where admins.channel_id = nullif(current_setting('centaur.slack_channel_id', true), '')
        )
    );
