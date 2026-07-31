create or replace function centaur_company_context_can_read_google_docs_observation(
    p_provider_email text
)
returns boolean
language sql
stable
as $$
    select lower(nullif(p_provider_email, '')) =
        lower(centaur_current_google_email())
$$;

revoke all on function centaur_company_context_can_read_google_docs_observation(text)
    from public;
grant execute on function centaur_current_google_email()
    to centaur_company_context_reader;
grant execute on function centaur_company_context_can_read_google_docs_observation(text)
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
        or (
            is_private
            and centaur_can_read_slack_user_conversation(
                centaur_current_slack_team_id(),
                channel_id
            )
        )
    );

drop policy if exists centaur_cc_reader_gdocs_observations_select
    on google_docs_sync_file_observations;
create policy centaur_cc_reader_gdocs_observations_select
    on google_docs_sync_file_observations
    for select
    to centaur_company_context_reader
    using (
        active
        and centaur_company_context_can_read_google_docs_observation(
            provider_email
        )
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
              and observations.active
              and centaur_company_context_can_read_google_docs_observation(
                  observations.provider_email
              )
        )
    );
