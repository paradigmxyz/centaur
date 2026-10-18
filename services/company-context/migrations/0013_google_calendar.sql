-- Google Calendar events synchronized through users' Google broker credentials.
-- Each credential syncs its own calendars and stores the events as Google
-- showed them to it, so documents are visible only to that credential's
-- Google identity and private events are redacted by Google itself.

create table company_context_system.google_calendars (
    broker_credential_id bigint not null,
    calendar_id text not null,
    provider_subject text not null,
    summary text not null default '',
    sync_token text not null default '',
    page_token text not null default '',
    -- Set while a full sync runs; events it does not list are removed when it
    -- completes.
    full_sync_started_at timestamptz,
    -- The earliest end time the latest full sync kept events from. Raising the
    -- history days moves the cutoff before it and starts a full sync.
    history_start timestamptz,
    last_success_at timestamptz,
    last_error text not null default '',
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, calendar_id),
    check (calendar_id <> ''),
    check (provider_subject <> '')
);

create table company_context_system.google_calendar_events (
    broker_credential_id bigint not null,
    calendar_id text not null,
    event_id text not null,
    title text not null default '',
    content_text text not null,
    url text not null default '',
    -- Recurring events are kept however long ago their series started.
    recurring boolean not null default false,
    starts_at timestamptz,
    ends_at timestamptz,
    organizer_email text not null default '',
    content_hash text not null,
    -- Incremented whenever the event must be republished, so that stale embed
    -- tasks can recognize that they were superseded.
    revision bigint not null default 1,
    embedding_status text not null default 'pending',
    last_error text not null default '',
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    published_at timestamptz,
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, calendar_id, event_id),
    foreign key (broker_credential_id, calendar_id)
        references company_context_system.google_calendars(broker_credential_id, calendar_id)
        on delete cascade,
    check (event_id <> ''),
    check (content_text <> ''),
    check (content_hash <> ''),
    check (embedding_status in ('pending', 'completed', 'rejected'))
);

create table company_context_data.google_calendar_documents (
    document_id text primary key,
    broker_credential_id bigint not null,
    -- The Google identity the event was synced for; only it sees the document.
    provider_subject text not null,
    calendar_id text not null,
    event_id text not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    url text not null default '',
    starts_at timestamptz,
    ends_at timestamptz,
    organizer_email text not null default '',
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (broker_credential_id, calendar_id, event_id, chunk_id),
    foreign key (broker_credential_id, calendar_id, event_id)
        references company_context_system.google_calendar_events(broker_credential_id, calendar_id, event_id)
        on delete cascade,
    check (document_id <> ''),
    check (provider_subject <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index google_calendar_documents_subject_idx
    on company_context_data.google_calendar_documents (provider_subject);

create index google_calendar_documents_starts_idx
    on company_context_data.google_calendar_documents (starts_at desc);

create index google_calendar_documents_bm25_idx
    on company_context_data.google_calendar_documents
    using bm25 (
        document_id,
        provider_subject,
        calendar_id,
        event_id,
        chunk_id,
        title,
        body,
        organizer_email,
        starts_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "provider_subject": {"tokenizer": {"type": "keyword"}},
            "calendar_id": {"tokenizer": {"type": "keyword"}},
            "event_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}},
            "organizer_email": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.google_calendar_document_embeddings (
    document_id text primary key
        references company_context_data.google_calendar_documents(document_id) on delete cascade,
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

-- Chunks with the same text, such as one shared event synced for several
-- users, reuse an existing vector.
create index google_calendar_document_embeddings_content_idx
    on company_context_data.google_calendar_document_embeddings (content_hash, model);

create index google_calendar_document_embeddings_hnsw_idx
    on company_context_data.google_calendar_document_embeddings
    using hnsw (embedding vector_cosine_ops);
