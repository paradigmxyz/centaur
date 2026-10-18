-- Google Calendar events synchronized through users' Google broker credentials.
-- Each calendar is synchronized once, through the credential observing it with
-- the most access, and each event is published as its own documents.

create table company_context_system.google_calendars (
    calendar_id text primary key,
    summary text not null default '',
    -- The credential whose view the sync and page tokens follow. A sync
    -- through another credential starts over with a full sync.
    sync_credential_id bigint,
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
    check (calendar_id <> '')
);

-- The calendars each credential lists, with its access role. Readers do not
-- see the details of private events, so only writers and owners see their
-- documents.
create table company_context_data.google_calendar_broker_observations (
    broker_credential_id bigint not null,
    calendar_id text not null
        references company_context_system.google_calendars(calendar_id) on delete cascade,
    provider_email text not null default '',
    provider_subject text not null default '',
    access_role text not null,
    active boolean not null default true,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, calendar_id),
    check (access_role in ('reader', 'writer', 'owner'))
);

create index google_calendar_broker_observations_active_calendar_idx
    on company_context_data.google_calendar_broker_observations (calendar_id)
    where active;

create table company_context_system.google_calendar_events (
    calendar_id text not null
        references company_context_system.google_calendars(calendar_id) on delete cascade,
    event_id text not null,
    title text not null default '',
    content_text text not null,
    url text not null default '',
    private boolean not null default false,
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
    primary key (calendar_id, event_id),
    check (event_id <> ''),
    check (content_text <> ''),
    check (content_hash <> ''),
    check (embedding_status in ('pending', 'completed', 'rejected'))
);

create index google_calendar_events_pending_idx
    on company_context_system.google_calendar_events (calendar_id)
    where embedding_status = 'pending';

create table company_context_data.google_calendar_documents (
    document_id text primary key,
    calendar_id text not null,
    event_id text not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    url text not null default '',
    private boolean not null default false,
    starts_at timestamptz,
    ends_at timestamptz,
    organizer_email text not null default '',
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (calendar_id, event_id, chunk_id),
    foreign key (calendar_id, event_id)
        references company_context_system.google_calendar_events(calendar_id, event_id)
        on delete cascade,
    check (document_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index google_calendar_documents_starts_idx
    on company_context_data.google_calendar_documents (starts_at desc);

create index google_calendar_documents_bm25_idx
    on company_context_data.google_calendar_documents
    using bm25 (
        document_id,
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

create index google_calendar_document_embeddings_hnsw_idx
    on company_context_data.google_calendar_document_embeddings
    using hnsw (embedding vector_cosine_ops);
