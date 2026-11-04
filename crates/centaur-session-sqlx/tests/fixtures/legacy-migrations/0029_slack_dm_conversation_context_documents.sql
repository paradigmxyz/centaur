create extension if not exists pg_search;

create table if not exists slack_dm_conversation_context_documents (
    document_id text primary key,
    home_team_id text not null,
    conversation_id text not null,
    conversation_type text not null default '',
    title text not null default '',
    body text not null default '',
    participant_user_ids text[] not null default array[]::text[],
    participant_labels text[] not null default array[]::text[],
    participant_count integer not null default 0,
    is_ext_shared boolean not null default false,
    last_seen_at timestamptz,
    source_updated_at timestamptz,
    content_hash text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (home_team_id, conversation_id),
    foreign key (home_team_id, conversation_id)
        references slack_dm_sync_conversations(home_team_id, conversation_id)
        on delete cascade,
    check (document_id <> ''),
    check (home_team_id <> ''),
    check (conversation_id <> '')
);

create index if not exists idx_slack_dm_conversation_context_documents_seen
    on slack_dm_conversation_context_documents (home_team_id, last_seen_at desc);

create index if not exists idx_slack_dm_conversation_context_documents_participants
    on slack_dm_conversation_context_documents using gin (participant_user_ids);

create index if not exists idx_slack_dm_conversation_context_documents_metadata
    on slack_dm_conversation_context_documents using gin (metadata);

drop index if exists idx_slack_dm_conversation_context_documents_bm25;

create index idx_slack_dm_conversation_context_documents_bm25
    on slack_dm_conversation_context_documents
    using bm25 (
        document_id,
        title,
        body,
        home_team_id,
        conversation_id,
        conversation_type,
        last_seen_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "home_team_id": {
                "tokenizer": {"type": "keyword"}
            },
            "conversation_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create or replace function centaur_refresh_slack_dm_conversation_context_document(
    p_home_team_id text,
    p_conversation_id text
)
returns void
language sql
as $$
    with participant_rows as (
        select
            c.home_team_id,
            c.conversation_id,
            c.conversation_type,
            c.is_ext_shared,
            c.last_seen_at,
            c.updated_at as conversation_updated_at,
            c.raw_payload as conversation_raw_payload,
            m.user_id,
            m.user_team_id,
            m.is_external,
            m.updated_at as member_updated_at,
            u.updated_at as user_updated_at,
            coalesce(
                nullif(m.raw_payload ->> 'display_name', ''),
                nullif(m.raw_payload #>> '{profile,display_name}', ''),
                nullif(u.display_name, ''),
                nullif(m.raw_payload ->> 'real_name', ''),
                nullif(m.raw_payload #>> '{profile,real_name}', ''),
                nullif(u.real_name, ''),
                nullif(m.raw_payload ->> 'name', ''),
                nullif(m.raw_payload #>> '{profile,name}', ''),
                nullif(u.user_name, ''),
                m.user_id
            ) as participant_label,
            nullif(
                concat_ws(
                    ' ',
                    m.user_id,
                    m.user_team_id,
                    m.raw_payload ->> 'display_name',
                    m.raw_payload #>> '{profile,display_name}',
                    u.display_name,
                    m.raw_payload ->> 'real_name',
                    m.raw_payload #>> '{profile,real_name}',
                    u.real_name,
                    m.raw_payload ->> 'name',
                    m.raw_payload #>> '{profile,name}',
                    u.user_name,
                    m.raw_payload ->> 'email',
                    m.raw_payload #>> '{profile,email}',
                    u.raw_payload ->> 'email',
                    u.raw_payload #>> '{profile,email}',
                    c.conversation_id
                ),
                ''
            ) as participant_search_text
        from slack_dm_sync_conversations c
        left join slack_dm_sync_conversation_members m
          on m.home_team_id = c.home_team_id
         and m.conversation_id = c.conversation_id
         and m.is_current_member
        left join slack_sync_users u
          on u.user_id = m.user_id
        where c.home_team_id = p_home_team_id
          and c.conversation_id = p_conversation_id
    ),
    aggregated as (
        select
            home_team_id,
            conversation_id,
            conversation_type,
            is_ext_shared,
            last_seen_at,
            conversation_updated_at,
            conversation_raw_payload,
            coalesce(
                array_agg(user_id order by participant_label, user_id)
                    filter (where user_id is not null),
                array[]::text[]
            ) as participant_user_ids,
            coalesce(
                array_agg(participant_label order by participant_label, user_id)
                    filter (where participant_label is not null),
                array[]::text[]
            ) as participant_labels,
            count(user_id)::integer as participant_count,
            string_agg(
                participant_search_text,
                E'\n'
                order by participant_label, user_id
            ) filter (where participant_search_text is not null) as participant_search_text,
            coalesce(
                jsonb_agg(
                    jsonb_build_object(
                        'user_id', user_id,
                        'user_team_id', user_team_id,
                        'label', participant_label,
                        'is_external', is_external
                    )
                    order by participant_label, user_id
                ) filter (where user_id is not null),
                '[]'::jsonb
            ) as participants,
            greatest(
                conversation_updated_at,
                coalesce(max(member_updated_at), conversation_updated_at),
                coalesce(max(user_updated_at), conversation_updated_at)
            ) as source_updated_at
        from participant_rows
        group by
            home_team_id,
            conversation_id,
            conversation_type,
            is_ext_shared,
            last_seen_at,
            conversation_updated_at,
            conversation_raw_payload
    ),
    projected as (
        select
            concat_ws(':', 'slack_dm_conversation', home_team_id, conversation_id)
                as document_id,
            home_team_id,
            conversation_id,
            conversation_type,
            case
                when participant_count > 0 and conversation_type = 'mpim'
                    then 'Slack group DM: ' || array_to_string(participant_labels, ', ')
                when participant_count > 0
                    then 'Slack DM: ' || array_to_string(participant_labels, ', ')
                when conversation_type = 'mpim' then 'Slack group DM'
                else 'Slack DM'
            end as title,
            trim(both E'\n' from concat_ws(
                E'\n',
                conversation_id,
                conversation_type,
                participant_search_text
            )) as body,
            participant_user_ids,
            participant_labels,
            participant_count,
            is_ext_shared,
            last_seen_at,
            source_updated_at,
            jsonb_build_object(
                'source', 'slack_dm_conversation',
                'home_team_id', home_team_id,
                'conversation_id', conversation_id,
                'conversation_type', conversation_type,
                'participant_count', participant_count,
                'participant_user_ids', to_jsonb(participant_user_ids),
                'participant_labels', to_jsonb(participant_labels),
                'participants', participants,
                'is_ext_shared', is_ext_shared,
                'raw_conversation', conversation_raw_payload
            ) as metadata
        from aggregated
    ),
    hashed as (
        select
            projected.*,
            md5(concat_ws(
                E'\x1f',
                title,
                body,
                array_to_string(participant_user_ids, E'\x1e'),
                array_to_string(participant_labels, E'\x1e'),
                coalesce(last_seen_at::text, ''),
                metadata::text
            )) as content_hash
        from projected
    )
    insert into slack_dm_conversation_context_documents (
        document_id,
        home_team_id,
        conversation_id,
        conversation_type,
        title,
        body,
        participant_user_ids,
        participant_labels,
        participant_count,
        is_ext_shared,
        last_seen_at,
        source_updated_at,
        content_hash,
        metadata,
        updated_at
    )
    select
        document_id,
        home_team_id,
        conversation_id,
        conversation_type,
        title,
        body,
        participant_user_ids,
        participant_labels,
        participant_count,
        is_ext_shared,
        last_seen_at,
        source_updated_at,
        content_hash,
        metadata,
        now()
    from hashed
    on conflict (document_id) do update set
        home_team_id = excluded.home_team_id,
        conversation_id = excluded.conversation_id,
        conversation_type = excluded.conversation_type,
        title = excluded.title,
        body = excluded.body,
        participant_user_ids = excluded.participant_user_ids,
        participant_labels = excluded.participant_labels,
        participant_count = excluded.participant_count,
        is_ext_shared = excluded.is_ext_shared,
        last_seen_at = excluded.last_seen_at,
        source_updated_at = excluded.source_updated_at,
        content_hash = excluded.content_hash,
        metadata = excluded.metadata,
        updated_at = now()
    where slack_dm_conversation_context_documents.content_hash
        is distinct from excluded.content_hash;
$$;

create or replace function centaur_refresh_slack_dm_conversation_context_from_conversation()
returns trigger
language plpgsql
as $$
begin
    perform centaur_refresh_slack_dm_conversation_context_document(
        new.home_team_id,
        new.conversation_id
    );
    return new;
end;
$$;

create or replace function centaur_refresh_slack_dm_conversation_context_from_member()
returns trigger
language plpgsql
as $$
declare
    row_ref record;
begin
    if tg_op = 'DELETE' then
        row_ref := old;
    else
        row_ref := new;
    end if;

    perform centaur_refresh_slack_dm_conversation_context_document(
        row_ref.home_team_id,
        row_ref.conversation_id
    );

    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end;
$$;

create or replace function centaur_refresh_slack_dm_conversation_context_from_user()
returns trigger
language plpgsql
as $$
declare
    row_ref record;
    membership record;
begin
    if tg_op = 'DELETE' then
        row_ref := old;
    else
        row_ref := new;
    end if;

    for membership in
        select home_team_id, conversation_id
        from slack_dm_sync_conversation_members
        where user_id = row_ref.user_id
          and is_current_member
    loop
        perform centaur_refresh_slack_dm_conversation_context_document(
            membership.home_team_id,
            membership.conversation_id
        );
    end loop;

    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end;
$$;

drop trigger if exists trg_slack_dm_conversations_refresh_context
    on slack_dm_sync_conversations;
create trigger trg_slack_dm_conversations_refresh_context
    after insert or update on slack_dm_sync_conversations
    for each row
    execute function centaur_refresh_slack_dm_conversation_context_from_conversation();

drop trigger if exists trg_slack_dm_members_refresh_conversation_context
    on slack_dm_sync_conversation_members;
create trigger trg_slack_dm_members_refresh_conversation_context
    after insert or update or delete on slack_dm_sync_conversation_members
    for each row
    execute function centaur_refresh_slack_dm_conversation_context_from_member();

drop trigger if exists trg_slack_sync_users_refresh_dm_conversation_context
    on slack_sync_users;
create trigger trg_slack_sync_users_refresh_dm_conversation_context
    after insert or update or delete on slack_sync_users
    for each row
    execute function centaur_refresh_slack_dm_conversation_context_from_user();

select centaur_refresh_slack_dm_conversation_context_document(
    conversations.home_team_id,
    conversations.conversation_id
)
from slack_dm_sync_conversations conversations;

grant select on slack_dm_conversation_context_documents
    to centaur_slack_reader, centaur_readonly;

alter table slack_dm_conversation_context_documents enable row level security;

drop policy if exists centaur_slack_dm_conversation_context_documents_reader_select
    on slack_dm_conversation_context_documents;
create policy centaur_slack_dm_conversation_context_documents_reader_select
    on slack_dm_conversation_context_documents
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id =
                slack_dm_conversation_context_documents.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id =
                slack_dm_conversation_context_documents.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_dm_conversation_context_documents;
create policy centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_dm_conversation_context_documents
    for select
    to centaur_readonly
    using (false);
