-- Expose retrieval-facing Drive documents to the company-context tool role.
-- api-rs also creates this role; create it here too so either service can
-- migrate first.
do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_company_context_reader') then
        create role centaur_company_context_reader nologin;
    end if;
end
$$;

-- A reader sees a file only while a live broker credential for the same Google
-- subject still observes it. Keep the policy keyed by the scalar file_id:
-- ParadeDB cannot combine paradedb.score with more complex RLS qualifiers on
-- the indexed relation. The GUC is read directly so this migration does not
-- depend on api-rs helper functions.
create or replace function company_context_data.google_drive_file_visible(p_file_id text)
returns boolean
language sql
stable
security definer
set search_path = pg_catalog
as $$
    select exists (
        select 1
        from company_context_system.google_drive_broker_observations observations
        where observations.file_id = p_file_id
          and observations.active
          and observations.provider_subject <> ''
          and observations.provider_subject = current_setting('centaur.google_subject', true)
    )
$$;

revoke all on function company_context_data.google_drive_file_visible(text) from public;
grant execute on function company_context_data.google_drive_file_visible(text)
    to centaur_company_context_reader;

grant usage on schema company_context_data to centaur_company_context_reader;
grant select on
    company_context_data.google_drive_documents,
    company_context_data.google_drive_document_embeddings
to centaur_company_context_reader;

alter table company_context_data.google_drive_documents enable row level security;
alter table company_context_data.google_drive_document_embeddings enable row level security;

create policy company_context_reader_select
    on company_context_data.google_drive_documents
    for select
    to centaur_company_context_reader
    using (company_context_data.google_drive_file_visible(file_id));

create policy company_context_reader_select
    on company_context_data.google_drive_document_embeddings
    for select
    to centaur_company_context_reader
    using (
        exists (
            select 1
            from company_context_data.google_drive_documents documents
            where documents.document_id = google_drive_document_embeddings.document_id
        )
    );
