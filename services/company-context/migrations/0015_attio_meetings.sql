-- Attio meetings, with their call recording transcripts, synchronized through
-- the Attio OAuth app's workspace credentials. Attio tokens are scoped to a
-- workspace, so each credential's meetings are stored with the workspace ID
-- (the credential's subject) and are visible to principals granted a
-- credential for that workspace.

create table company_context_system.attio_checkpoints (
    scope_id text primary key,
    -- When the latest complete listing started; the next one rereads from a
    -- window before it.
    watermark_time timestamptz,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (scope_id <> '')
);

create table company_context_system.attio_meetings (
    broker_credential_id bigint not null,
    meeting_id text not null,
    provider_subject text not null,
    title text not null default '',
    url text not null default '',
    starts_at timestamptz,
    ends_at timestamptz,
    participants jsonb not null default '[]'::jsonb,
    linked_records jsonb not null default '[]'::jsonb,
    content_text text not null default '',
    content_hash text not null default '',
    -- Incremented whenever the meeting must be republished, so that stale
    -- embed tasks can recognize that they were superseded.
    revision bigint not null default 1,
    embedding_status text not null default 'pending',
    last_error text not null default '',
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    published_at timestamptz,
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, meeting_id),
    check (meeting_id <> ''),
    check (provider_subject <> ''),
    check (embedding_status in ('pending', 'completed', 'rejected'))
);

create table company_context_data.attio_documents (
    document_id text primary key,
    broker_credential_id bigint not null,
    meeting_id text not null,
    -- The Attio workspace ID; principals granted a credential for it see the
    -- document.
    provider_subject text not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    url text not null default '',
    starts_at timestamptz,
    ends_at timestamptz,
    participants jsonb not null default '[]'::jsonb,
    linked_records jsonb not null default '[]'::jsonb,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (broker_credential_id, meeting_id, chunk_id),
    foreign key (broker_credential_id, meeting_id)
        references company_context_system.attio_meetings(broker_credential_id, meeting_id)
        on delete cascade,
    check (document_id <> ''),
    check (provider_subject <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index attio_documents_subject_idx
    on company_context_data.attio_documents (provider_subject);

create index attio_documents_starts_idx
    on company_context_data.attio_documents (starts_at desc);

create index attio_documents_bm25_idx
    on company_context_data.attio_documents
    using bm25 (
        document_id,
        provider_subject,
        meeting_id,
        chunk_id,
        title,
        body,
        starts_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "provider_subject": {"tokenizer": {"type": "keyword"}},
            "meeting_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.attio_document_embeddings (
    document_id text primary key references company_context_data.attio_documents(document_id) on delete cascade,
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

create index attio_document_embeddings_hnsw_idx
    on company_context_data.attio_document_embeddings
    using hnsw (embedding vector_cosine_ops);
