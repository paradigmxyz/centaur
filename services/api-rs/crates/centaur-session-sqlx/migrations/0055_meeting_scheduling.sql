create table if not exists centaur_meeting_occurrences (
    occurrence_key text primary key,
    cadence_id text,
    request_id text,
    title text not null default '',
    status text not null default 'pending',
    requested_start timestamptz not null,
    actual_start timestamptz,
    duration_minutes integer not null,
    time_zone text not null default 'UTC',
    organizer_calendar_key text not null,
    organizer_calendar_id text not null default '',
    calendar_event_id text not null default '',
    calendar_html_link text not null default '',
    zoom_meeting_id text not null default '',
    zoom_join_url text not null default '',
    attendee_emails text[] not null default array[]::text[],
    version integer not null default 1,
    metadata jsonb not null default '{}'::jsonb,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint centaur_meeting_occurrences_status_check
        check (status in ('pending', 'booked', 'blocked', 'completed', 'cancelled')),
    constraint centaur_meeting_occurrences_duration_check
        check (duration_minutes > 0)
);

create unique index if not exists idx_centaur_meeting_occurrences_request
    on centaur_meeting_occurrences (request_id)
    where request_id is not null;

create index if not exists idx_centaur_meeting_occurrences_cadence
    on centaur_meeting_occurrences (cadence_id, requested_start desc);

create index if not exists idx_centaur_meeting_occurrences_calendar_event
    on centaur_meeting_occurrences (calendar_event_id)
    where calendar_event_id <> '';

create index if not exists idx_centaur_meeting_occurrences_zoom_meeting
    on centaur_meeting_occurrences (zoom_meeting_id)
    where zoom_meeting_id <> '';

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_meeting_scheduler') then
        create role centaur_meeting_scheduler
            nologin
            nosuperuser
            nocreatedb
            nocreaterole
            noinherit
            noreplication;
    end if;
end
$$;

grant usage on schema public to centaur_meeting_scheduler;
grant select, insert, update on centaur_meeting_occurrences to centaur_meeting_scheduler;
grant centaur_meeting_scheduler to current_user;
