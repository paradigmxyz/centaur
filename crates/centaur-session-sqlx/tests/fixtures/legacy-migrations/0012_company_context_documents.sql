create extension if not exists pg_search;

create table if not exists company_context_documents (
    document_id text primary key,
    source text not null,
    source_type text not null,
    source_document_id text not null,
    source_chunk_id text not null default '',
    parent_document_id text references company_context_documents(document_id) on delete cascade,
    title text not null default '',
    body text not null default '',
    url text not null default '',
    author_id text not null default '',
    author_name text not null default '',
    access_scope text not null default 'company',
    occurred_at timestamptz,
    source_updated_at timestamptz,
    content_hash text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (source <> ''),
    check (source_type <> ''),
    check (source_document_id <> ''),
    unique (source, source_type, source_document_id, source_chunk_id)
);

create index if not exists idx_company_context_documents_source_time
    on company_context_documents (source, source_type, occurred_at desc);

create index if not exists idx_company_context_documents_parent
    on company_context_documents (parent_document_id);

create index if not exists idx_company_context_documents_updated
    on company_context_documents (source_updated_at desc);

create index if not exists idx_company_context_documents_metadata
    on company_context_documents using gin (metadata);

drop index if exists idx_company_context_documents_bm25;

create index idx_company_context_documents_bm25
    on company_context_documents
    using bm25 (
        document_id,
        title,
        body,
        source,
        source_type,
        access_scope,
        occurred_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );
