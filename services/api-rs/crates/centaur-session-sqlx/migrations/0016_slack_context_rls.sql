do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_slack_reader') then
        create role centaur_slack_reader nologin;
    end if;

    if not exists (select 1 from pg_roles where rolname = 'centaur_slack_admin') then
        create role centaur_slack_admin nologin;
    end if;
end
$$;

grant usage on schema public to centaur_slack_reader, centaur_slack_admin;

grant select on slack_sync_channels to centaur_slack_reader, centaur_slack_admin;
grant select on slack_sync_users to centaur_slack_reader, centaur_slack_admin;
grant select on slack_sync_messages to centaur_slack_reader, centaur_slack_admin;
grant select on company_context_documents to centaur_slack_reader, centaur_slack_admin;

alter table slack_sync_messages enable row level security;

alter table company_context_documents enable row level security;

drop policy if exists centaur_slack_messages_admin_select on slack_sync_messages;
create policy centaur_slack_messages_admin_select
    on slack_sync_messages
    for select
    to centaur_slack_admin
    using (true);

drop policy if exists centaur_slack_messages_reader_select on slack_sync_messages;
create policy centaur_slack_messages_reader_select
    on slack_sync_messages
    for select
    to centaur_slack_reader
    using (
        channel_id = nullif(current_setting('centaur.slack_channel_id', true), '')
    );

drop policy if exists centaur_context_docs_admin_select on company_context_documents;
create policy centaur_context_docs_admin_select
    on company_context_documents
    for select
    to centaur_slack_admin
    using (true);

drop policy if exists centaur_context_docs_reader_select on company_context_documents;
create policy centaur_context_docs_reader_select
    on company_context_documents
    for select
    to centaur_slack_reader
    using (
        source <> 'slack'
        or metadata ->> 'channel_id' = nullif(current_setting('centaur.slack_channel_id', true), '')
    );
