-- The Linear, Google Drive, Google Calendar, and Attio projections write rows
-- with no Slack channel and access_scope = 'company', so
-- centaur_cc_reader_documents_select never matches them. Show them to the
-- sessions that may read public Slack channels; a session limited to explicit
-- channel grants still sees only those channels. Rows with another
-- access_scope stay hidden until a policy covers them.
drop policy if exists centaur_cc_reader_non_slack_documents_select
    on company_context_documents;
create policy centaur_cc_reader_non_slack_documents_select
    on company_context_documents
    for select
    to centaur_company_context_reader
    using (
        source <> 'slack'
        and access_scope = 'company'
        and (select centaur_company_context_include_public_slack())
    );
