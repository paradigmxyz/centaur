-- Slack users, for rendering names, and channel documents projected from the
-- staged message history. Each channel day is rendered into chunks; every
-- chunk is its own document with its own embedding. Like the initial Drive and
-- Granola corpora, no reader grants or RLS policies are added yet.

-- Users of each workspace a live Slack credential belongs to. updated_at only
-- changes when a rendered field changes, so projections can tell when a name
-- they rendered is stale.
create table company_context_system.slack_users (
    user_id text primary key,
    team_id text not null,
    name text not null default '',
    real_name text not null default '',
    display_name text not null default '',
    is_bot boolean not null default false,
    deleted boolean not null default false,
    first_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (user_id <> ''),
    check (team_id <> '')
);

create index slack_users_team_idx on company_context_system.slack_users (team_id);

-- Replies belong to the day their thread started, so a thread stays in one
-- channel day document.
alter table company_context_system.slack_messages
    add column projection_day date generated always as (
        (to_timestamp(coalesce(thread_ts, message_ts)::double precision) at time zone 'UTC')::date
    ) stored;

create index slack_messages_projection_day_idx
    on company_context_system.slack_messages (conversation_id, projection_day);

-- The latest rendering of each channel day. A day is rendered again when its
-- messages, channel name, or rendered user names change, or when the
-- projection version changes.
create table company_context_system.slack_channel_days (
    conversation_id text not null
        references company_context_system.slack_conversations(conversation_id) on delete cascade,
    day date not null,
    projection_version integer not null,
    channel_name text not null default '',
    -- Users whose names the rendering includes.
    user_ids text[] not null default '{}',
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
    primary key (conversation_id, day),
    check (content_hash <> ''),
    check (embedding_status in ('pending', 'completed', 'rejected'))
);

create table company_context_data.slack_documents (
    document_id text primary key,
    conversation_id text not null,
    day date not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    channel_name text not null default '',
    conversation_kind text not null,
    first_message_at timestamptz not null,
    last_message_at timestamptz not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (conversation_id, day, chunk_id),
    foreign key (conversation_id, day)
        references company_context_system.slack_channel_days(conversation_id, day) on delete cascade,
    check (document_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index slack_documents_last_message_idx
    on company_context_data.slack_documents (last_message_at desc);

create index slack_documents_bm25_idx
    on company_context_data.slack_documents
    using bm25 (
        document_id,
        conversation_id,
        chunk_id,
        title,
        body,
        channel_name,
        conversation_kind,
        last_message_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "conversation_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}},
            "conversation_kind": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.slack_document_embeddings (
    document_id text primary key references company_context_data.slack_documents(document_id) on delete cascade,
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

create index slack_document_embeddings_hnsw_idx
    on company_context_data.slack_document_embeddings
    using hnsw (embedding vector_cosine_ops);
