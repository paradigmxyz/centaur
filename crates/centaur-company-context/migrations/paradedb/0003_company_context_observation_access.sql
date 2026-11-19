-- Broker observations are the only source of Drive access, so they live with
-- the data they gate. company_context_system stays internal to the service.
-- The unused Drive ACL snapshot is dropped.
drop table company_context_data.google_drive_document_access;

alter table company_context_system.google_drive_broker_observations
    set schema company_context_data;

create or replace function company_context_data.google_drive_file_visible(p_file_id text)
returns boolean
language sql
stable
security definer
set search_path = pg_catalog
as $$
    select exists (
        select 1
        from company_context_data.google_drive_broker_observations observations
        where observations.file_id = p_file_id
          and observations.active
          and observations.provider_subject <> ''
          and observations.provider_subject = current_setting('centaur.google_subject', true)
    )
$$;
