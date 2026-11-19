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

-- Events are published as they are synced, one keyword-searchable document
-- each. Calendar events are short and full of exact terms, so they are not
-- embedded.
create table company_context_data.google_calendar_documents (
    document_id text primary key,
    broker_credential_id bigint not null,
    calendar_id text not null,
    event_id text not null,
    -- The Google identity the event was synced for; only it sees the document.
    provider_subject text not null,
    title text not null default '',
    body text not null,
    url text not null default '',
    -- Recurring events are kept however long ago their series started.
    recurring boolean not null default false,
    starts_at timestamptz,
    ends_at timestamptz,
    organizer_email text not null default '',
    content_hash text not null,
    first_seen_at timestamptz not null default now(),
    -- A full sync removes the events it did not list.
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (broker_credential_id, calendar_id, event_id),
    foreign key (broker_credential_id, calendar_id)
        references company_context_system.google_calendars(broker_credential_id, calendar_id)
        on delete cascade,
    check (document_id <> ''),
    check (event_id <> ''),
    check (provider_subject <> ''),
    check (body <> ''),
    check (content_hash <> '')
);

create index google_calendar_documents_subject_idx
    on company_context_data.google_calendar_documents (provider_subject);

create index google_calendar_documents_starts_idx
    on company_context_data.google_calendar_documents (starts_at desc);

-- Built-in full-text search. Ranking reads the stored vector instead of
-- re-parsing every matching body. Title terms carry weight A and body terms
-- weight D; the body is capped so a very large document cannot exceed the
-- 1 MB tsvector limit and fail its write.
alter table company_context_data.google_calendar_documents
    add column search_vector tsvector generated always as (
        setweight(to_tsvector('english', title), 'A')
        || setweight(to_tsvector('english', left(body, 500000)), 'D')
    ) stored;
create index google_calendar_documents_search_vector_idx
    on company_context_data.google_calendar_documents using gin (search_vector);
