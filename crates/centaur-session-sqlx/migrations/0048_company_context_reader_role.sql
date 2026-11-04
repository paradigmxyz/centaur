do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_company_context_reader') then
        create role centaur_company_context_reader nologin;
    end if;
end
$$;

grant usage on schema public to centaur_company_context_reader;
grant centaur_company_context_reader to current_user;

create or replace function centaur_current_slack_history_channel_ids()
returns text[]
language sql
stable
as $$
    select coalesce(
        array(
            select jsonb_array_elements_text(
                coalesce(
                    nullif(
                        current_setting('centaur.slack_history_channel_ids', true),
                        ''
                    )::jsonb,
                    '[]'::jsonb
                )
            )
        ),
        array[]::text[]
    )
$$;

create or replace function centaur_company_context_include_public_slack()
returns boolean
language sql
stable
as $$
    select current_setting('centaur.slack_include_public', true) = 'true'
$$;

revoke all on function centaur_current_slack_history_channel_ids() from public;
revoke all on function centaur_company_context_include_public_slack() from public;

grant execute on function centaur_current_slack_channel_id()
    to centaur_company_context_reader;
grant execute on function centaur_current_slack_team_id()
    to centaur_company_context_reader;
grant execute on function centaur_current_slack_user_id()
    to centaur_company_context_reader;
grant execute on function centaur_current_google_subject()
    to centaur_company_context_reader;
grant execute on function centaur_current_slack_user_email()
    to centaur_company_context_reader;
grant execute on function centaur_granola_current_user_can_read(text[])
    to centaur_company_context_reader;
grant execute on function centaur_can_read_slack_user_conversation(text, text)
    to centaur_company_context_reader;
grant execute on function centaur_current_slack_history_channel_ids()
    to centaur_company_context_reader;
grant execute on function centaur_company_context_include_public_slack()
    to centaur_company_context_reader;

grant select on
    slack_sync_channels,
    company_context_documents,
    google_docs_sync_file_observations,
    google_docs_context_documents,
    granola_context_documents,
    slack_private_context_documents,
    slack_private_conversation_context_documents
to centaur_company_context_reader;

drop policy if exists centaur_cc_reader_channels_select
    on slack_sync_channels;
create policy centaur_cc_reader_channels_select
    on slack_sync_channels
    for select
    to centaur_company_context_reader
    using (
        channel_id = centaur_current_slack_channel_id()
        or channel_id = any(
            (select centaur_current_slack_history_channel_ids())::text[]
        )
        or (
            (select centaur_company_context_include_public_slack())
            and not is_private
        )
    );

drop policy if exists centaur_cc_reader_documents_select
    on company_context_documents;
create policy centaur_cc_reader_documents_select
    on company_context_documents
    for select
    to centaur_company_context_reader
    using (
        source = 'slack'
        and metadata ->> 'channel_id' in (
            select channels.channel_id
            from slack_sync_channels channels
        )
    );

drop policy if exists centaur_cc_reader_gdocs_observations_select
    on google_docs_sync_file_observations;
create policy centaur_cc_reader_gdocs_observations_select
    on google_docs_sync_file_observations
    for select
    to centaur_company_context_reader
    using (
        provider_subject <> ''
        and provider_subject = centaur_current_google_subject()
        and active
    );

drop policy if exists centaur_cc_reader_gdocs_documents_select
    on google_docs_context_documents;
create policy centaur_cc_reader_gdocs_documents_select
    on google_docs_context_documents
    for select
    to centaur_company_context_reader
    using (
        exists (
            select 1
            from google_docs_sync_file_observations observations
            where observations.file_id = google_docs_context_documents.file_id
              and observations.provider_subject = centaur_current_google_subject()
              and observations.active
        )
    );

drop policy if exists centaur_cc_reader_granola_documents_select
    on granola_context_documents;
create policy centaur_cc_reader_granola_documents_select
    on granola_context_documents
    for select
    to centaur_company_context_reader
    using (centaur_granola_current_user_can_read(access_emails));

drop policy if exists centaur_cc_reader_private_docs_select
    on slack_private_context_documents;
create policy centaur_cc_reader_private_docs_select
    on slack_private_context_documents
    for select
    to centaur_company_context_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_cc_reader_private_conversation_docs_select
    on slack_private_conversation_context_documents;
create policy centaur_cc_reader_private_conversation_docs_select
    on slack_private_conversation_context_documents
    for select
    to centaur_company_context_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));
