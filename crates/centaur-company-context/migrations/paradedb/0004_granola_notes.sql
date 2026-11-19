-- Granola meeting notes synchronized through each user's Granola MCP OAuth
-- credential. Transcripts are not synchronized; the Granola tool fetches them
-- on demand. Like the initial Drive corpus, no reader grants or RLS policies
-- are added yet; the corpus is validated before it is exposed to retrieval.

create table company_context_system.granola_checkpoints (
    scope_id text primary key,
    watermark_time timestamptz,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (scope_id <> '')
);

create table company_context_system.granola_notes (
    note_id text primary key,
    title text not null default '',
    owner jsonb not null default '{}'::jsonb,
    attendees jsonb not null default '[]'::jsonb,
    summary_markdown text not null default '',
    content_text text not null default '',
    content_hash text not null default '',
    source_created_at timestamptz,
    -- Incremented whenever the note must be republished or removed, so that
    -- stale embed tasks can recognize that they were superseded.
    revision bigint not null default 1,
    embedding_status text not null default 'pending',
    last_error text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    published_at timestamptz,
    updated_at timestamptz not null default now(),
    check (note_id <> ''),
    check (embedding_status in ('pending', 'completed', 'rejected', 'deleted'))
);

create table company_context_data.granola_broker_observations (
    broker_credential_id bigint not null,
    note_id text not null,
    provider_email text not null default '',
    provider_subject text not null default '',
    active boolean not null default true,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, note_id),
    check (note_id <> '')
);

create index granola_broker_observations_active_note_idx
    on company_context_data.granola_broker_observations (note_id)
    where active;

create table company_context_data.granola_documents (
    document_id text primary key,
    note_id text not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    owner_email text not null default '',
    owner_name text not null default '',
    attendees jsonb not null default '[]'::jsonb,
    occurred_at timestamptz,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (note_id, chunk_id),
    check (document_id <> ''),
    check (note_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index granola_documents_occurred_idx
    on company_context_data.granola_documents (occurred_at desc);

create index granola_documents_bm25_idx
    on company_context_data.granola_documents
    using bm25 (
        document_id,
        note_id,
        chunk_id,
        title,
        body,
        owner_email,
        owner_name,
        occurred_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "note_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}},
            "owner_email": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.granola_document_embeddings (
    document_id text primary key references company_context_data.granola_documents(document_id) on delete cascade,
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

create index granola_document_embeddings_hnsw_idx
    on company_context_data.granola_document_embeddings
    using hnsw (embedding vector_cosine_ops);
