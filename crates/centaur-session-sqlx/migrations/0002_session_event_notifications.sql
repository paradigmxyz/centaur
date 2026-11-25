create or replace function notify_session_event_insert()
returns trigger
language plpgsql
as $$
begin
    perform pg_notify(
        'centaur_session_events',
        json_build_object(
            'thread_key', new.thread_key,
            'event_id', new.event_id
        )::text
    );

    return new;
end;
$$;

drop trigger if exists session_events_notify_insert on session_events;

create trigger session_events_notify_insert
after insert on session_events
for each row
execute function notify_session_event_insert();
