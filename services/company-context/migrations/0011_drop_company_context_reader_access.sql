-- Company context v2 is read only through the query API, so the tool reader
-- role loses its direct access and the Drive tables drop row-level security.
-- The role itself stays: api-rs still grants it access to its own tables.
drop policy company_context_reader_select on company_context_data.google_drive_documents;
drop policy company_context_reader_select on company_context_data.google_drive_document_embeddings;
drop policy company_context_v2_query_select on company_context_data.google_drive_documents;
drop policy company_context_v2_query_select on company_context_data.google_drive_document_embeddings;

alter table company_context_data.google_drive_documents disable row level security;
alter table company_context_data.google_drive_document_embeddings disable row level security;

drop function company_context_data.google_drive_file_visible(text);

revoke select on
    company_context_data.google_drive_documents,
    company_context_data.google_drive_document_embeddings
from centaur_company_context_reader;
revoke usage on schema company_context_data from centaur_company_context_reader;
