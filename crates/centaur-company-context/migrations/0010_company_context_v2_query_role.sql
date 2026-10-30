-- The query API reads as this role: read-only access to company_context_data
-- and nothing else. It cannot log in; the service switches to it with
-- SET LOCAL ROLE for each query transaction.
do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_company_context_v2_query') then
        create role centaur_company_context_v2_query nologin;
    end if;
end
$$;

grant centaur_company_context_v2_query to current_user;

grant usage on schema company_context_data to centaur_company_context_v2_query;
grant select on all tables in schema company_context_data to centaur_company_context_v2_query;
alter default privileges in schema company_context_data
    grant select on tables to centaur_company_context_v2_query;

-- These tables already enforce row-level security for the tool's reader role.
-- The query API filters visibility in SQL for now, so it sees every row;
-- per-principal policies for this role replace these later.
create policy company_context_v2_query_select
    on company_context_data.google_drive_documents
    for select
    to centaur_company_context_v2_query
    using (true);

create policy company_context_v2_query_select
    on company_context_data.google_drive_document_embeddings
    for select
    to centaur_company_context_v2_query
    using (true);
