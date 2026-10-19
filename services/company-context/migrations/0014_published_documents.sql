-- Documents that producers publish through the ingestion API instead of being
-- synchronized from a source. Each publishing source owns its documents, keyed
-- by the producer's external ID. Published documents are public: every
-- principal can read them.

-- Full syncs let a producer replace a source's documents: those not published
-- during the sync are removed when it completes.
create table company_context_system.published_syncs (
    sync_id uuid primary key,
    source text not null,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    check (source <> '')
);

create index published_syncs_source_started_idx
    on company_context_system.published_syncs (source, started_at desc);

create table company_context_system.published_items (
    source text not null,
    external_id text not null,
    -- Hash of the published fields; unchanged republications are not embedded
    -- again.
    content_hash text not null,
    -- The full sync that last published the item, if any.
    sync_id uuid references company_context_system.published_syncs(sync_id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (source, external_id),
    check (source <> ''),
    check (external_id <> ''),
    check (content_hash <> '')
);

create table company_context_data.published_documents (
    document_id text primary key,
    source text not null,
    external_id text not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    url text not null default '',
    author text not null default '',
    occurred_at timestamptz,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (source, external_id, chunk_id),
    foreign key (source, external_id)
        references company_context_system.published_items(source, external_id)
        on delete cascade,
    check (document_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index published_documents_occurred_idx
    on company_context_data.published_documents (occurred_at desc);

create index published_documents_bm25_idx
    on company_context_data.published_documents
    using bm25 (
        document_id,
        source,
        external_id,
        chunk_id,
        title,
        body,
        author,
        occurred_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "source": {"tokenizer": {"type": "keyword"}},
            "external_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.published_document_embeddings (
    document_id text primary key references company_context_data.published_documents(document_id) on delete cascade,
    model text not null,
    dimensions integer not null,
    content_hash text not null,
    embedding vector(1536) not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (model <> ''),
    check (dimensions = 1536),
    check (content_hash <> '')
);

create index published_document_embeddings_hnsw_idx
    on company_context_data.published_document_embeddings
    using hnsw (embedding vector_cosine_ops);
