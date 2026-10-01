-- Generalize the existing user-scoped DM store before adding private channels.
-- Table renames preserve all DM/MPIM rows, foreign keys, indexes, grants, RLS,
-- and triggers in place.
alter table slack_dm_sync_conversations
    rename to slack_private_sync_conversations;
alter table slack_dm_sync_conversation_members
    rename to slack_private_sync_conversation_members;
alter table slack_dm_sync_runs
    rename to slack_private_sync_runs;
alter table slack_dm_sync_messages
    rename to slack_private_sync_messages;
alter table slack_dm_sync_message_attachments
    rename to slack_private_sync_message_attachments;
alter table slack_dm_sync_checkpoints
    rename to slack_private_sync_checkpoints;
alter table slack_dm_sync_backfill_jobs
    rename to slack_private_sync_backfill_jobs;
alter table slack_dm_context_documents
    rename to slack_private_context_documents;
alter table slack_dm_conversation_context_documents
    rename to slack_private_conversation_context_documents;

-- ParadeDB ties BM25 metadata to the indexed relation name, so rebuild these
-- two indexes after the table rename instead of relying on the index OID alone.
drop index if exists idx_slack_dm_context_documents_bm25;
drop index if exists idx_slack_private_context_documents_bm25;
create index idx_slack_private_context_documents_bm25
    on slack_private_context_documents
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

drop index if exists idx_slack_dm_conversation_context_documents_bm25;
drop index if exists idx_slack_private_conversation_context_documents_bm25;
create index idx_slack_private_conversation_context_documents_bm25
    on slack_private_conversation_context_documents
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

-- PL/pgSQL function bodies retain relation names as source text. Recreate any
-- existing projection function with the new table names so the triggers that
-- moved with the tables continue to work after the rename.
do $$
declare
    fn record;
    old_definition text;
    new_definition text;
begin
    for fn in
        select p.oid
        from pg_proc p
        join pg_namespace n on n.oid = p.pronamespace
        where n.nspname = 'public'
          and p.prokind = 'f'
          and pg_get_functiondef(p.oid) like '%slack_dm_%'
    loop
        old_definition := pg_get_functiondef(fn.oid);
        new_definition := old_definition;
        new_definition := replace(new_definition,
            'slack_dm_conversation_context_documents',
            'slack_private_conversation_context_documents');
        new_definition := replace(new_definition,
            'slack_dm_sync_conversation_members',
            'slack_private_sync_conversation_members');
        new_definition := replace(new_definition,
            'slack_dm_sync_message_attachments',
            'slack_private_sync_message_attachments');
        new_definition := replace(new_definition,
            'slack_dm_sync_backfill_jobs',
            'slack_private_sync_backfill_jobs');
        new_definition := replace(new_definition,
            'slack_dm_sync_conversations',
            'slack_private_sync_conversations');
        new_definition := replace(new_definition,
            'slack_dm_sync_checkpoints',
            'slack_private_sync_checkpoints');
        new_definition := replace(new_definition,
            'slack_dm_sync_messages',
            'slack_private_sync_messages');
        new_definition := replace(new_definition,
            'slack_dm_sync_runs',
            'slack_private_sync_runs');
        new_definition := replace(new_definition,
            'slack_dm_context_documents',
            'slack_private_context_documents');

        if new_definition is distinct from old_definition then
            execute new_definition;
        end if;
    end loop;
end
$$;

alter table slack_private_sync_conversations
    drop constraint if exists slack_dm_sync_conversations_conversation_type_check;

alter table slack_private_sync_conversations
    add constraint slack_private_sync_conversations_conversation_type_check
    check (conversation_type in ('im', 'mpim', 'private_channel'));

-- Centralize access checks for every user-scoped Slack conversation. A user
-- retains access until a successful membership reconciliation marks the row
-- inactive; incomplete membership responses are rejected by the console sync.
create or replace function centaur_can_read_slack_user_conversation(
    p_home_team_id text,
    p_conversation_id text
)
returns boolean
language sql
stable
security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1
        from public.slack_private_sync_conversation_members members
        where members.home_team_id = p_home_team_id
          and members.conversation_id = p_conversation_id
          and members.home_team_id = public.centaur_current_slack_team_id()
          and members.user_id = public.centaur_current_slack_user_id()
          and members.is_current_member
    )
$$;

revoke all on function centaur_can_read_slack_user_conversation(text, text) from public;
grant execute on function centaur_can_read_slack_user_conversation(text, text)
    to centaur_slack_reader, centaur_readonly;

drop policy if exists centaur_slack_dm_conversations_reader_select
    on slack_private_sync_conversations;
create policy centaur_slack_dm_conversations_reader_select
    on slack_private_sync_conversations for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_conversations_select
    on slack_private_sync_conversations;
create policy centaur_readonly_slack_dm_sync_conversations_select
    on slack_private_sync_conversations for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_members_reader_select
    on slack_private_sync_conversation_members;
create policy centaur_slack_dm_members_reader_select
    on slack_private_sync_conversation_members for select to centaur_slack_reader
    using (
        user_id = centaur_current_slack_user_id()
        and centaur_can_read_slack_user_conversation(home_team_id, conversation_id)
    );

drop policy if exists centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_private_sync_conversation_members;
create policy centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_private_sync_conversation_members for select to centaur_readonly
    using (
        user_id = centaur_current_slack_user_id()
        and centaur_can_read_slack_user_conversation(home_team_id, conversation_id)
    );

drop policy if exists centaur_slack_dm_messages_reader_select
    on slack_private_sync_messages;
create policy centaur_slack_dm_messages_reader_select
    on slack_private_sync_messages for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_messages_select
    on slack_private_sync_messages;
create policy centaur_readonly_slack_dm_sync_messages_select
    on slack_private_sync_messages for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_attachments_reader_select
    on slack_private_sync_message_attachments;
create policy centaur_slack_dm_attachments_reader_select
    on slack_private_sync_message_attachments for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_private_sync_message_attachments;
create policy centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_private_sync_message_attachments for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_checkpoints_reader_select
    on slack_private_sync_checkpoints;
create policy centaur_slack_dm_checkpoints_reader_select
    on slack_private_sync_checkpoints for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_private_sync_checkpoints;
create policy centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_private_sync_checkpoints for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_context_documents_reader_select
    on slack_private_context_documents;
create policy centaur_slack_dm_context_documents_reader_select
    on slack_private_context_documents for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_context_documents_select
    on slack_private_context_documents;
create policy centaur_readonly_slack_dm_context_documents_select
    on slack_private_context_documents for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_conversation_context_documents_reader_select
    on slack_private_conversation_context_documents;
create policy centaur_slack_dm_conversation_context_documents_reader_select
    on slack_private_conversation_context_documents for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_private_conversation_context_documents;
create policy centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_private_conversation_context_documents for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

-- The existing projection triggers still populate the private conversation
-- tables. These BEFORE triggers give private-channel rows accurate titles and
-- metadata without duplicating the projection pipeline.
create or replace function centaur_label_slack_private_channel_message_document()
returns trigger
language plpgsql
as $$
declare
    channel_name text;
begin
    if new.conversation_type <> 'private_channel' then
        return new;
    end if;

    select nullif(conversations.raw_payload ->> 'name', '')
      into channel_name
      from slack_private_sync_conversations conversations
     where conversations.home_team_id = new.home_team_id
       and conversations.conversation_id = new.conversation_id;

    new.title := 'Slack private channel: #' || coalesce(channel_name, new.conversation_id);
    new.metadata := new.metadata || jsonb_build_object(
        'source', 'slack_private_channel',
        'channel_id', new.conversation_id,
        'channel_name', coalesce(channel_name, '')
    );
    new.content_hash := md5(concat_ws(
        E'\x1f', new.title, new.body, new.permalink,
        coalesce(new.occurred_at::text, ''), new.metadata::text
    ));
    return new;
end;
$$;

drop trigger if exists trg_label_slack_private_channel_message_document
    on slack_private_context_documents;
create trigger trg_label_slack_private_channel_message_document
    before insert or update on slack_private_context_documents
    for each row
    execute function centaur_label_slack_private_channel_message_document();

create or replace function centaur_label_slack_private_channel_conversation_document()
returns trigger
language plpgsql
as $$
declare
    channel_name text;
begin
    if new.conversation_type <> 'private_channel' then
        return new;
    end if;

    select nullif(conversations.raw_payload ->> 'name', '')
      into channel_name
      from slack_private_sync_conversations conversations
     where conversations.home_team_id = new.home_team_id
       and conversations.conversation_id = new.conversation_id;

    new.title := 'Slack private channel: #' || coalesce(channel_name, new.conversation_id);
    new.body := concat_ws(E'\n', channel_name, new.body);
    new.metadata := new.metadata || jsonb_build_object(
        'source', 'slack_private_channel',
        'channel_id', new.conversation_id,
        'channel_name', coalesce(channel_name, '')
    );
    new.content_hash := md5(concat_ws(
        E'\x1f', new.title, new.body,
        array_to_string(new.participant_user_ids, E'\x1e'),
        array_to_string(new.participant_labels, E'\x1e'),
        coalesce(new.last_seen_at::text, ''), new.metadata::text
    ));
    return new;
end;
$$;

drop trigger if exists trg_label_slack_private_channel_conversation_document
    on slack_private_conversation_context_documents;
create trigger trg_label_slack_private_channel_conversation_document
    before insert or update on slack_private_conversation_context_documents
    for each row
    execute function centaur_label_slack_private_channel_conversation_document();
