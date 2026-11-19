-- Files attached to projected Slack channel messages. Each file is extracted
-- once, however many messages share it, and every chunk of its text is its
-- own document with its own embedding. Downloaded bytes are not stored. Like
-- the Slack channel documents, no reader grants or RLS policies are added yet.

create table company_context_system.slack_files (
    file_id text primary key,
    name text not null default '',
    title text not null default '',
    mimetype text not null default '',
    filetype text not null default '',
    mode text not null default '',
    size bigint,
    user_id text not null default '',
    permalink text not null default '',
    source_created_at timestamptz,
    -- Changes when Slack reports different content, so the file is
    -- extracted again.
    source_version text not null,
    content_hash text not null default '',
    extraction_status text not null,
    embedding_status text not null default 'pending',
    last_error text not null default '',
    -- When the file's current extraction or embedding was last enqueued, so
    -- that unfinished work is enqueued again once it is overdue.
    task_requested_at timestamptz,
    -- Credentials Slack refused this version of the file to.
    denied_credential_ids bigint[] not null default '{}',
    -- The file object as Slack sent it.
    metadata jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    published_at timestamptz,
    updated_at timestamptz not null default now(),
    check (file_id <> ''),
    check (source_version <> ''),
    check (extraction_status in ('pending', 'completed', 'rejected', 'deleted')),
    check (embedding_status in ('pending', 'completed', 'rejected'))
);

-- The messages that share each file. Shares go with their message, and a
-- file is removed once no message shares it.
create table company_context_system.slack_file_shares (
    file_id text not null
        references company_context_system.slack_files(file_id) on delete cascade,
    conversation_id text not null,
    message_ts text not null,
    primary key (file_id, conversation_id, message_ts),
    foreign key (conversation_id, message_ts)
        references company_context_system.slack_messages(conversation_id, message_ts)
        on delete cascade
);

create index slack_file_shares_message_idx
    on company_context_system.slack_file_shares (conversation_id, message_ts);

create table company_context_system.slack_file_chunks (
    file_id text not null
        references company_context_system.slack_files(file_id) on delete cascade,
    chunk_id text not null,
    ordinal integer not null,
    body text not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    primary key (file_id, chunk_id),
    unique (file_id, ordinal),
    check (chunk_id <> ''),
    check (ordinal >= 0),
    check (body <> ''),
    check (content_hash <> '')
);

create table company_context_data.slack_file_documents (
    document_id text primary key,
    file_id text not null
        references company_context_system.slack_files(file_id) on delete cascade,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    mimetype text not null default '',
    filetype text not null default '',
    url text not null default '',
    author_id text not null default '',
    source_created_at timestamptz,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (file_id, chunk_id),
    check (document_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index slack_file_documents_created_idx
    on company_context_data.slack_file_documents (source_created_at desc);

create index slack_file_documents_bm25_idx
    on company_context_data.slack_file_documents
    using bm25 (
        document_id,
        file_id,
        chunk_id,
        title,
        body,
        mimetype,
        filetype,
        source_created_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "file_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}},
            "mimetype": {"tokenizer": {"type": "keyword"}},
            "filetype": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.slack_file_document_embeddings (
    document_id text primary key
        references company_context_data.slack_file_documents(document_id) on delete cascade,
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

create index slack_file_document_embeddings_hnsw_idx
    on company_context_data.slack_file_document_embeddings
    using hnsw (embedding vector_cosine_ops);
