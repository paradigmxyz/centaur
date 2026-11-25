-- Who can read each custom document. Writers set readers with the document:
-- 'public', 'principal:<id>', or 'role:<id>', where <id> is the Console
-- opaque ID (prn_..., role_...) or foreign ID. A document is visible to a
-- principal when any reader matches the principal, one of its Console roles,
-- or everyone; a document without readers is visible to no one.

create function company_context_system.custom_document_readers_valid(readers text[])
returns boolean
language sql
immutable
as $$
    select coalesce(bool_and(
        coalesce(reader ~ '^(public|(principal|role):[A-Za-z0-9._~-]+)$', false)
    ), true)
    from unnest(readers) reader
$$;

-- Custom documents were public until now; keep existing ones so. New rows
-- must name their readers.
alter table company_context_system.custom_documents
    add column readers text[] not null default '{public}',
    add constraint custom_documents_readers_check
        check (company_context_system.custom_document_readers_valid(readers));
alter table company_context_system.custom_documents
    alter column readers drop default;

-- Chunks carry their row's readers so queries can filter by them.
alter table company_context_data.custom_document_chunks
    add column readers text[] not null default '{public}';
alter table company_context_data.custom_document_chunks
    alter column readers drop default;

create index custom_document_chunks_readers_idx
    on company_context_data.custom_document_chunks using gin (readers);

-- Changing a row's readers applies to its published chunks at once, without
-- waiting for or causing a projection. Runs as the owner, so writers need
-- access to company_context_system only.
create function company_context_system.custom_documents_readers()
returns trigger
language plpgsql
security definer
set search_path = ''
as $$
begin
    update company_context_data.custom_document_chunks
    set readers = new.readers
    where source = new.source and external_id = new.external_id;
    return null;
end
$$;

create trigger custom_documents_readers
    after update of readers on company_context_system.custom_documents
    for each row
    when (old.readers is distinct from new.readers)
    execute function company_context_system.custom_documents_readers();
