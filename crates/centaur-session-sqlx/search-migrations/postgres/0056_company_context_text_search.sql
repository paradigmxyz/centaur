-- Built-in PostgreSQL full-text search for company context documents.
--
-- api-rs refuses this backend while BM25 indexes from earlier core migrations
-- remain, so these tables carry no other keyword index.

-- Ranking reads the stored vector instead of re-parsing every matching body.
-- Title terms carry weight A and body terms weight D. The body is capped so a
-- very large document cannot exceed the 1 MB tsvector limit and fail its write.
alter table company_context_documents
    add column search_vector tsvector generated always as (
        setweight(to_tsvector('english', title), 'A')
        || setweight(to_tsvector('english', left(body, 500000)), 'D')
    ) stored;
create index idx_company_context_documents_search_vector
    on company_context_documents using gin (search_vector);

alter table google_docs_context_documents
    add column search_vector tsvector generated always as (
        setweight(to_tsvector('english', title), 'A')
        || setweight(to_tsvector('english', left(body, 500000)), 'D')
    ) stored;
create index idx_google_docs_context_documents_search_vector
    on google_docs_context_documents using gin (search_vector);

alter table granola_context_documents
    add column search_vector tsvector generated always as (
        setweight(to_tsvector('english', title), 'A')
        || setweight(to_tsvector('english', left(body, 500000)), 'D')
    ) stored;
create index idx_granola_context_documents_search_vector
    on granola_context_documents using gin (search_vector);

alter table slack_private_context_documents
    add column search_vector tsvector generated always as (
        setweight(to_tsvector('english', title), 'A')
        || setweight(to_tsvector('english', left(body, 500000)), 'D')
    ) stored;
create index idx_slack_private_context_documents_search_vector
    on slack_private_context_documents using gin (search_vector);

alter table slack_private_conversation_context_documents
    add column search_vector tsvector generated always as (
        setweight(to_tsvector('english', title), 'A')
        || setweight(to_tsvector('english', left(body, 500000)), 'D')
    ) stored;
create index idx_slack_private_conversation_context_documents_search_vector
    on slack_private_conversation_context_documents using gin (search_vector);
