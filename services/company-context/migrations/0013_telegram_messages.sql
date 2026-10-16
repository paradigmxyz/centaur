-- Telegram group messages, projected into chat day documents. Telegram allows
-- only one update consumer per bot token, so the Telegram ingress (telegrambot)
-- writes each message from a connected group here as it arrives; this service
-- only projects and expires them. Connected groups are company-wide: the query
-- API shows their documents to every principal.

create table company_context_system.telegram_messages (
    chat_id bigint not null,
    message_id bigint not null,
    -- Forum topic, when the group has topics.
    topic_id bigint,
    chat_title text not null default '',
    sender_id bigint,
    sender_name text not null default '',
    sender_username text not null default '',
    reply_to_message_id bigint,
    -- The message text, or a media message's caption.
    text text not null default '',
    -- Attached media as [{"kind", "name", "mime_type"}]. File contents are not
    -- stored.
    media jsonb not null default '[]'::jsonb,
    sent_at timestamptz not null,
    received_at timestamptz not null default now(),
    projection_day date generated always as ((sent_at at time zone 'UTC')::date) stored,
    primary key (chat_id, message_id),
    check (jsonb_typeof(media) = 'array')
);

create index telegram_messages_projection_day_idx
    on company_context_system.telegram_messages (chat_id, projection_day);

-- Expiry deletes by day across chats.
create index telegram_messages_expiry_idx
    on company_context_system.telegram_messages (projection_day);

-- The latest rendering of each chat day, as for Slack channel days.
create table company_context_system.telegram_chat_days (
    chat_id bigint not null,
    day date not null,
    projection_version integer not null,
    chat_title text not null default '',
    message_count integer not null default 0,
    title text not null default '',
    -- Rendered chunks: [{"body", "first_message_at", "last_message_at"}].
    chunks jsonb not null default '[]'::jsonb,
    content_hash text not null,
    rendered_at timestamptz not null,
    -- Incremented whenever the rendered content changes, so that stale embed
    -- tasks can recognize that they were superseded.
    revision bigint not null default 1,
    embedding_status text not null default 'pending',
    last_error text not null default '',
    published_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (chat_id, day),
    check (content_hash <> ''),
    check (embedding_status in ('pending', 'completed', 'rejected'))
);

create table company_context_data.telegram_documents (
    document_id text primary key,
    chat_id bigint not null,
    day date not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    chat_title text not null default '',
    first_message_at timestamptz not null,
    last_message_at timestamptz not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (chat_id, day, chunk_id),
    foreign key (chat_id, day)
        references company_context_system.telegram_chat_days(chat_id, day) on delete cascade,
    check (document_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index telegram_documents_last_message_idx
    on company_context_data.telegram_documents (last_message_at desc);

create index telegram_documents_bm25_idx
    on company_context_data.telegram_documents
    using bm25 (
        document_id,
        chunk_id,
        title,
        body,
        chat_title,
        last_message_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.telegram_document_embeddings (
    document_id text primary key
        references company_context_data.telegram_documents(document_id) on delete cascade,
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

create index telegram_document_embeddings_hnsw_idx
    on company_context_data.telegram_document_embeddings
    using hnsw (embedding vector_cosine_ops);
