create schema if not exists company_context_system;
create schema if not exists company_context_data;

create extension if not exists vector;
create extension if not exists pg_search;

create table company_context_system.google_drive_checkpoints (
    scope_id text primary key,
    initial_start_page_token text not null default '',
    initial_page_token text not null default '',
    initial_scan_completed boolean not null default false,
    changes_page_token text not null default '',
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (scope_id <> '')
);

create table company_context_system.google_drive_files (
    file_id text primary key,
    name text not null default '',
    mime_type text not null default '',
    drive_id text not null default '',
    web_view_link text not null default '',
    source_version text not null default '',
    observation_key text not null default '',
    source_created_at timestamptz,
    source_modified_at timestamptz,
    content_hash text not null default '',
    extraction_status text not null default 'pending',
    embedding_status text not null default 'pending',
    last_error text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    published_at timestamptz,
    updated_at timestamptz not null default now(),
    check (file_id <> ''),
    check (extraction_status in ('pending', 'completed', 'failed', 'rejected', 'deleted')),
    check (embedding_status in ('pending', 'completed', 'failed', 'rejected', 'deleted'))
);

create index google_drive_files_modified_idx
    on company_context_system.google_drive_files (source_modified_at desc);

create table company_context_system.google_drive_broker_observations (
    broker_credential_id bigint not null,
    file_id text not null,
    provider_email text not null default '',
    provider_subject text not null default '',
    observation_key text not null default '',
    active boolean not null default true,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, file_id),
    check (file_id <> '')
);

create index google_drive_broker_observations_active_file_idx
    on company_context_system.google_drive_broker_observations (file_id)
    where active;

create table company_context_system.google_drive_chunks (
    file_id text not null references company_context_system.google_drive_files(file_id) on delete cascade,
    chunk_id text not null,
    ordinal integer not null,
    body text not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (file_id, chunk_id),
    unique (file_id, ordinal),
    check (chunk_id <> ''),
    check (ordinal >= 0),
    check (body <> ''),
    check (content_hash <> '')
);

create table company_context_data.google_drive_document_access (
    file_id text not null,
    permission_id text not null,
    permission_type text not null default '',
    role text not null default '',
    email_address text not null default '',
    domain text not null default '',
    allow_file_discovery boolean,
    source_version text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (file_id, permission_id),
    check (file_id <> ''),
    check (permission_id <> '')
);

create index google_drive_document_access_email_idx
    on company_context_data.google_drive_document_access (lower(email_address))
    where email_address <> '';

create index google_drive_document_access_domain_idx
    on company_context_data.google_drive_document_access (lower(domain))
    where domain <> '';

create table company_context_data.google_drive_documents (
    document_id text primary key,
    file_id text not null,
    chunk_id text not null,
    document_type text not null,
    mime_type text not null,
    title text not null default '',
    body text not null,
    url text not null default '',
    drive_id text not null default '',
    page_start integer,
    page_end integer,
    source_created_at timestamptz,
    source_modified_at timestamptz,
    source_version text not null default '',
    content_hash text not null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (file_id, chunk_id),
    check (document_id <> ''),
    check (file_id <> ''),
    check (chunk_id <> ''),
    check (document_type <> ''),
    check (mime_type <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index google_drive_documents_file_idx
    on company_context_data.google_drive_documents (file_id);

create index google_drive_documents_modified_idx
    on company_context_data.google_drive_documents (source_modified_at desc);

create index google_drive_documents_metadata_idx
    on company_context_data.google_drive_documents using gin (metadata);

create index google_drive_documents_bm25_idx
    on company_context_data.google_drive_documents
    using bm25 (
        document_id,
        file_id,
        chunk_id,
        document_type,
        mime_type,
        title,
        body,
        drive_id,
        source_modified_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "file_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}},
            "mime_type": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.google_drive_document_embeddings (
    document_id text primary key references company_context_data.google_drive_documents(document_id) on delete cascade,
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

create index google_drive_document_embeddings_hnsw_idx
    on company_context_data.google_drive_document_embeddings
    using hnsw (embedding vector_cosine_ops);

-- Deliberately no reader grants or RLS policies yet. The initial daemon can
-- populate and validate this corpus without exposing it through retrieval.
