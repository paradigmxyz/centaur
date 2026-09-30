create extension if not exists pg_search;

create table if not exists slack_dm_context_documents (
    document_id text primary key,
    home_team_id text not null,
    conversation_id text not null,
    message_ts text not null,
    conversation_type text not null default '',
    thread_ts text,
    parent_message_ts text,
    is_thread_root boolean not null default false,
    user_id text not null default '',
    user_team_id text,
    bot_id text not null default '',
    message_type text not null default 'message',
    message_subtype text,
    title text not null default '',
    body text not null default '',
    permalink text not null default '',
    occurred_at timestamptz,
    source_updated_at timestamptz,
    content_hash text not null default '',
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (home_team_id, conversation_id, message_ts),
    foreign key (home_team_id, conversation_id, message_ts)
        references slack_dm_sync_messages(home_team_id, conversation_id, message_ts)
        on delete cascade,
    check (document_id <> ''),
    check (home_team_id <> ''),
    check (conversation_id <> ''),
    check (message_ts <> '')
);

create index if not exists idx_slack_dm_context_documents_conversation_time
    on slack_dm_context_documents (home_team_id, conversation_id, occurred_at desc);

create index if not exists idx_slack_dm_context_documents_user_time
    on slack_dm_context_documents (home_team_id, user_id, occurred_at desc);

create index if not exists idx_slack_dm_context_documents_metadata
    on slack_dm_context_documents using gin (metadata);

drop index if exists idx_slack_dm_context_documents_bm25;

create index idx_slack_dm_context_documents_bm25
    on slack_dm_context_documents
    using bm25 (
        document_id,
        title,
        body,
        home_team_id,
        conversation_id,
        conversation_type,
        user_id,
        bot_id,
        message_type,
        message_subtype,
        occurred_at,
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
            },
            "user_id": {
                "tokenizer": {"type": "keyword"}
            },
            "bot_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create or replace function centaur_refresh_slack_dm_context_document(
    p_home_team_id text,
    p_conversation_id text,
    p_message_ts text
)
returns void
language sql
as $$
    with attachment_summary as (
        select
            a.home_team_id,
            a.conversation_id,
            a.message_ts,
            count(*)::integer as attachment_count,
            coalesce(
                jsonb_agg(
                    jsonb_build_object(
                        'slack_file_id', a.slack_file_id,
                        'name', a.name,
                        'title', a.title,
                        'mimetype', a.mimetype,
                        'filetype', a.filetype,
                        'size_bytes', a.size_bytes,
                        'permalink', a.permalink,
                        'download_status', a.download_status
                    )
                    order by a.slack_file_id
                ),
                '[]'::jsonb
            ) as attachments,
            string_agg(
                nullif(
                    concat_ws(
                        ' ',
                        nullif(a.title, ''),
                        nullif(a.name, ''),
                        nullif(a.mimetype, ''),
                        nullif(a.filetype, '')
                    ),
                    ''
                ),
                E'\n'
                order by a.slack_file_id
            ) as attachment_text,
            max(a.updated_at) as attachments_updated_at
        from slack_dm_sync_message_attachments a
        where a.home_team_id = p_home_team_id
          and a.conversation_id = p_conversation_id
          and a.message_ts = p_message_ts
        group by a.home_team_id, a.conversation_id, a.message_ts
    ),
    projected as (
        select
            concat_ws(':', 'slack_dm', m.home_team_id, m.conversation_id, m.message_ts)
                as document_id,
            m.home_team_id,
            m.conversation_id,
            m.message_ts,
            c.conversation_type,
            m.thread_ts,
            m.parent_message_ts,
            m.is_thread_root,
            m.user_id,
            m.user_team_id,
            m.bot_id,
            m.message_type,
            m.message_subtype,
            case
                when c.conversation_type = 'mpim' then 'Slack group DM'
                else 'Slack DM'
            end as title,
            trim(both E'\n' from concat_ws(
                E'\n',
                nullif(m.text, ''),
                nullif(attachment_summary.attachment_text, '')
            )) as body,
            m.permalink,
            m.occurred_at,
            greatest(
                m.updated_at,
                c.updated_at,
                coalesce(attachment_summary.attachments_updated_at, m.updated_at)
            ) as source_updated_at,
            jsonb_build_object(
                'source', 'slack_dm',
                'home_team_id', m.home_team_id,
                'conversation_id', m.conversation_id,
                'conversation_type', c.conversation_type,
                'message_ts', m.message_ts,
                'thread_ts', m.thread_ts,
                'parent_message_ts', m.parent_message_ts,
                'is_thread_root', m.is_thread_root,
                'user_id', m.user_id,
                'user_team_id', m.user_team_id,
                'bot_id', m.bot_id,
                'message_type', m.message_type,
                'message_subtype', m.message_subtype,
                'reply_count', m.reply_count,
                'latest_reply_ts', m.latest_reply_ts,
                'attachment_count', coalesce(attachment_summary.attachment_count, 0),
                'attachments', coalesce(attachment_summary.attachments, '[]'::jsonb)
            ) as metadata
        from slack_dm_sync_messages m
        join slack_dm_sync_conversations c
          on c.home_team_id = m.home_team_id
         and c.conversation_id = m.conversation_id
        left join attachment_summary
          on attachment_summary.home_team_id = m.home_team_id
         and attachment_summary.conversation_id = m.conversation_id
         and attachment_summary.message_ts = m.message_ts
        where m.home_team_id = p_home_team_id
          and m.conversation_id = p_conversation_id
          and m.message_ts = p_message_ts
    ),
    hashed as (
        select
            projected.*,
            md5(concat_ws(
                E'\x1f',
                title,
                body,
                permalink,
                coalesce(occurred_at::text, ''),
                metadata::text
            )) as content_hash
        from projected
    )
    insert into slack_dm_context_documents (
        document_id,
        home_team_id,
        conversation_id,
        message_ts,
        conversation_type,
        thread_ts,
        parent_message_ts,
        is_thread_root,
        user_id,
        user_team_id,
        bot_id,
        message_type,
        message_subtype,
        title,
        body,
        permalink,
        occurred_at,
        source_updated_at,
        content_hash,
        metadata,
        updated_at
    )
    select
        document_id,
        home_team_id,
        conversation_id,
        message_ts,
        conversation_type,
        thread_ts,
        parent_message_ts,
        is_thread_root,
        user_id,
        user_team_id,
        bot_id,
        message_type,
        message_subtype,
        title,
        body,
        permalink,
        occurred_at,
        source_updated_at,
        content_hash,
        metadata,
        now()
    from hashed
    on conflict (document_id) do update set
        home_team_id = excluded.home_team_id,
        conversation_id = excluded.conversation_id,
        message_ts = excluded.message_ts,
        conversation_type = excluded.conversation_type,
        thread_ts = excluded.thread_ts,
        parent_message_ts = excluded.parent_message_ts,
        is_thread_root = excluded.is_thread_root,
        user_id = excluded.user_id,
        user_team_id = excluded.user_team_id,
        bot_id = excluded.bot_id,
        message_type = excluded.message_type,
        message_subtype = excluded.message_subtype,
        title = excluded.title,
        body = excluded.body,
        permalink = excluded.permalink,
        occurred_at = excluded.occurred_at,
        source_updated_at = excluded.source_updated_at,
        content_hash = excluded.content_hash,
        metadata = excluded.metadata,
        updated_at = now()
    where slack_dm_context_documents.content_hash is distinct from excluded.content_hash;
$$;

create or replace function centaur_refresh_slack_dm_context_document_from_message()
returns trigger
language plpgsql
as $$
begin
    perform centaur_refresh_slack_dm_context_document(
        new.home_team_id,
        new.conversation_id,
        new.message_ts
    );
    return new;
end;
$$;

create or replace function centaur_refresh_slack_dm_context_document_from_attachment()
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

    perform centaur_refresh_slack_dm_context_document(
        row_ref.home_team_id,
        row_ref.conversation_id,
        row_ref.message_ts
    );

    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end;
$$;

drop trigger if exists trg_slack_dm_messages_refresh_context
    on slack_dm_sync_messages;
create trigger trg_slack_dm_messages_refresh_context
    after insert or update on slack_dm_sync_messages
    for each row
    execute function centaur_refresh_slack_dm_context_document_from_message();

drop trigger if exists trg_slack_dm_attachments_refresh_context
    on slack_dm_sync_message_attachments;
create trigger trg_slack_dm_attachments_refresh_context
    after insert or update or delete on slack_dm_sync_message_attachments
    for each row
    execute function centaur_refresh_slack_dm_context_document_from_attachment();

select centaur_refresh_slack_dm_context_document(
    messages.home_team_id,
    messages.conversation_id,
    messages.message_ts
)
from slack_dm_sync_messages messages;

grant select on slack_dm_context_documents
    to centaur_slack_reader, centaur_readonly;

alter table slack_dm_context_documents enable row level security;

drop policy if exists centaur_slack_dm_context_documents_reader_select
    on slack_dm_context_documents;
create policy centaur_slack_dm_context_documents_reader_select
    on slack_dm_context_documents
    for select
    to centaur_slack_reader
    using (
        exists (
            select 1
            from slack_dm_sync_conversation_members members
            where members.home_team_id = slack_dm_context_documents.home_team_id
              and members.home_team_id = centaur_current_slack_team_id()
              and members.conversation_id = slack_dm_context_documents.conversation_id
              and members.user_id = centaur_current_slack_user_id()
              and members.is_current_member
        )
    );

drop policy if exists centaur_readonly_slack_dm_context_documents_select
    on slack_dm_context_documents;
create policy centaur_readonly_slack_dm_context_documents_select
    on slack_dm_context_documents
    for select
    to centaur_readonly
    using (false);
