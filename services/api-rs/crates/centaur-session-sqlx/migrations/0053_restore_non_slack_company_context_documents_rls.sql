drop policy if exists centaur_context_docs_reader_select
    on company_context_documents;
create policy centaur_context_docs_reader_select
    on company_context_documents
    for select
    to centaur_slack_reader
    using (
        source <> 'slack'
        or (
            source = 'slack'
            and metadata ->> 'channel_id' = centaur_current_slack_channel_id()
        )
    );
