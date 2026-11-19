-- Custom documents created through the HTTP API record the Console principal
-- that created them, by opaque ID (prn_...). Only that principal can read,
-- replace, or delete them through the API. Rows written directly to the
-- table leave it null and cannot be changed through the API.

alter table company_context_system.custom_documents
    add column written_by text;

-- Lists a principal's documents in a source, newest first; API IDs are
-- UUIDv7s, which sort by creation time.
create index custom_documents_written_by_idx
    on company_context_system.custom_documents (written_by, source, external_id)
    where written_by is not null;

-- Chunks carry their row's writer so query results can show who wrote them.
-- A row's writer never changes.
alter table company_context_data.custom_document_chunks
    add column written_by text;
