create table if not exists google_calendar_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    calendars_requested jsonb not null default '[]'::jsonb,
    calendars_synced jsonb not null default '[]'::jsonb,
    calendars_failed jsonb not null default '[]'::jsonb,
    calendars_seen integer not null default 0,
    calendars_upserted integer not null default 0,
    events_seen integer not null default 0,
    events_upserted integer not null default 0,
    events_cancelled integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_google_calendar_sync_runs_started
    on google_calendar_sync_runs (started_at desc);

create table if not exists google_calendar_sync_calendars (
    calendar_id text primary key,
    summary text not null default '',
    description text not null default '',
    location text not null default '',
    time_zone text not null default '',
    access_role text not null default '',
    is_primary boolean not null default false,
    is_selected boolean not null default false,
    is_hidden boolean not null default false,
    background_color text not null default '',
    foreground_color text not null default '',
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references google_calendar_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_google_calendar_sync_calendars_summary
    on google_calendar_sync_calendars (summary);

create table if not exists google_calendar_sync_events (
    calendar_id text not null references google_calendar_sync_calendars(calendar_id) on delete cascade,
    event_id text not null,
    i_cal_uid text not null default '',
    status text not null default '',
    summary text not null default '',
    description text not null default '',
    location text not null default '',
    html_link text not null default '',
    creator jsonb not null default '{}'::jsonb,
    organizer jsonb not null default '{}'::jsonb,
    attendees jsonb not null default '[]'::jsonb,
    start_payload jsonb not null default '{}'::jsonb,
    end_payload jsonb not null default '{}'::jsonb,
    start_at timestamptz,
    end_at timestamptz,
    is_all_day boolean not null default false,
    recurring_event_id text not null default '',
    original_start jsonb not null default '{}'::jsonb,
    transparency text not null default '',
    visibility text not null default '',
    event_type text not null default '',
    sequence integer not null default 0,
    source_created_at timestamptz,
    source_updated_at timestamptz,
    content_text text not null default '',
    content_hash text not null default '',
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references google_calendar_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now(),
    primary key (calendar_id, event_id)
);

create index if not exists idx_google_calendar_sync_events_start
    on google_calendar_sync_events (start_at desc);

create index if not exists idx_google_calendar_sync_events_updated
    on google_calendar_sync_events (source_updated_at desc);

create index if not exists idx_google_calendar_sync_events_text
    on google_calendar_sync_events
    using gin (to_tsvector('english', coalesce(content_text, '')));

create index if not exists idx_google_calendar_sync_events_attendees
    on google_calendar_sync_events using gin (attendees);

create table if not exists google_calendar_sync_checkpoints (
    calendar_id text primary key references google_calendar_sync_calendars(calendar_id) on delete cascade,
    sync_token text not null default '',
    watermark_time timestamptz,
    last_run_id text references google_calendar_sync_runs(run_id) on delete set null,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
