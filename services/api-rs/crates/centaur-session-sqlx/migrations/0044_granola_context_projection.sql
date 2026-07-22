-- Granola's OAuth sync writes normalized source rows. Project those rows into
-- the dedicated search table in the same transaction, rather than routing
-- them through the generic company_context_documents projection.

create or replace function centaur_refresh_granola_context_document(
    p_note_id text
)
returns void
language sql
as $$
    with attendee_rows as (
        select
            coalesce(
                nullif(btrim(attendee ->> 'name'), ''),
                nullif(btrim(attendee ->> 'display_name'), ''),
                nullif(btrim(attendee ->> 'email'), ''),
                nullif(btrim(attendee ->> 'id'), '')
            ) as attendee_label
        from granola_sync_notes notes
        cross join lateral jsonb_array_elements(notes.attendees) attendee
        where notes.note_id = p_note_id
    ),
    projected as (
        select
            concat_ws(':', 'granola', 'note', notes.note_id) as document_id,
            notes.note_id,
            coalesce(nullif(notes.title, ''), 'Granola note') as title,
            notes.content_text as body,
            notes.url,
            notes.owner_id,
            notes.owner_email,
            notes.owner_name,
            notes.access_emails,
            coalesce(
                array_agg(distinct attendee_rows.attendee_label order by attendee_rows.attendee_label)
                    filter (where attendee_rows.attendee_label is not null),
                array[]::text[]
            ) as attendee_labels,
            coalesce(notes.source_created_at, notes.source_updated_at) as occurred_at,
            notes.source_updated_at,
            jsonb_build_object(
                'source', 'granola',
                'source_type', 'granola_note',
                'note_id', notes.note_id,
                'owner_id', notes.owner_id,
                'owner_email', notes.owner_email,
                'owner_name', notes.owner_name,
                'attendee_count', jsonb_array_length(notes.attendees),
                'calendar_event', notes.calendar_event
            ) as metadata
        from granola_sync_notes notes
        left join attendee_rows on true
        where notes.note_id = p_note_id
        group by
            notes.note_id,
            notes.title,
            notes.content_text,
            notes.url,
            notes.owner_id,
            notes.owner_email,
            notes.owner_name,
            notes.access_emails,
            notes.source_created_at,
            notes.source_updated_at,
            notes.attendees,
            notes.calendar_event
    ),
    hashed as (
        select
            projected.*,
            md5(concat_ws(
                E'\\x1f',
                title,
                body,
                url,
                owner_id,
                owner_email,
                owner_name,
                array_to_string(access_emails, E'\\x1e'),
                array_to_string(attendee_labels, E'\\x1e'),
                coalesce(occurred_at::text, ''),
                coalesce(source_updated_at::text, ''),
                metadata::text
            )) as content_hash
        from projected
    )
    insert into granola_context_documents (
        document_id,
        note_id,
        title,
        body,
        url,
        owner_id,
        owner_email,
        owner_name,
        access_emails,
        attendee_labels,
        occurred_at,
        source_updated_at,
        content_hash,
        metadata,
        updated_at
    )
    select
        document_id,
        note_id,
        title,
        body,
        url,
        owner_id,
        owner_email,
        owner_name,
        access_emails,
        attendee_labels,
        occurred_at,
        source_updated_at,
        content_hash,
        metadata,
        now()
    from hashed
    on conflict (document_id) do update set
        note_id = excluded.note_id,
        title = excluded.title,
        body = excluded.body,
        url = excluded.url,
        owner_id = excluded.owner_id,
        owner_email = excluded.owner_email,
        owner_name = excluded.owner_name,
        access_emails = excluded.access_emails,
        attendee_labels = excluded.attendee_labels,
        occurred_at = excluded.occurred_at,
        source_updated_at = excluded.source_updated_at,
        content_hash = excluded.content_hash,
        metadata = excluded.metadata,
        updated_at = now()
    where granola_context_documents.content_hash is distinct from excluded.content_hash;
$$;

create or replace function centaur_refresh_granola_context_document_from_note()
returns trigger
language plpgsql
as $$
begin
    perform centaur_refresh_granola_context_document(new.note_id);
    return new;
end;
$$;

drop trigger if exists trg_granola_sync_notes_refresh_context
    on granola_sync_notes;
create trigger trg_granola_sync_notes_refresh_context
    after insert or update on granola_sync_notes
    for each row
    execute function centaur_refresh_granola_context_document_from_note();

select centaur_refresh_granola_context_document(note_id)
from granola_sync_notes;
