-- Extend the user-scoped Slack conversation store to private channels. The
-- existing slack_dm_* names are retained for compatibility, but all rows in
-- these tables are private Slack conversations protected by membership RLS.

alter table slack_dm_sync_conversations
    drop constraint if exists slack_dm_sync_conversations_conversation_type_check;

alter table slack_dm_sync_conversations
    add constraint slack_dm_sync_conversations_conversation_type_check
    check (conversation_type in ('im', 'mpim', 'private_channel'));

-- Private-channel access fails closed when no successful membership
-- reconciliation has refreshed the user's row recently. DMs and MPIMs retain
-- their existing behavior because Slack exposes their participants directly.
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
        from public.slack_dm_sync_conversation_members members
        join public.slack_dm_sync_conversations conversations
          on conversations.home_team_id = members.home_team_id
         and conversations.conversation_id = members.conversation_id
        where members.home_team_id = p_home_team_id
          and members.conversation_id = p_conversation_id
          and members.home_team_id = public.centaur_current_slack_team_id()
          and members.user_id = public.centaur_current_slack_user_id()
          and members.is_current_member
          and (
              conversations.conversation_type <> 'private_channel'
              or members.last_seen_at >= now() - interval '30 minutes'
          )
    )
$$;

revoke all on function centaur_can_read_slack_user_conversation(text, text) from public;
grant execute on function centaur_can_read_slack_user_conversation(text, text)
    to centaur_slack_reader, centaur_readonly;

drop policy if exists centaur_slack_dm_conversations_reader_select
    on slack_dm_sync_conversations;
create policy centaur_slack_dm_conversations_reader_select
    on slack_dm_sync_conversations for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_conversations_select
    on slack_dm_sync_conversations;
create policy centaur_readonly_slack_dm_sync_conversations_select
    on slack_dm_sync_conversations for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_members_reader_select
    on slack_dm_sync_conversation_members;
create policy centaur_slack_dm_members_reader_select
    on slack_dm_sync_conversation_members for select to centaur_slack_reader
    using (
        user_id = centaur_current_slack_user_id()
        and centaur_can_read_slack_user_conversation(home_team_id, conversation_id)
    );

drop policy if exists centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_dm_sync_conversation_members;
create policy centaur_readonly_slack_dm_sync_conversation_members_select
    on slack_dm_sync_conversation_members for select to centaur_readonly
    using (
        user_id = centaur_current_slack_user_id()
        and centaur_can_read_slack_user_conversation(home_team_id, conversation_id)
    );

drop policy if exists centaur_slack_dm_messages_reader_select
    on slack_dm_sync_messages;
create policy centaur_slack_dm_messages_reader_select
    on slack_dm_sync_messages for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_messages_select
    on slack_dm_sync_messages;
create policy centaur_readonly_slack_dm_sync_messages_select
    on slack_dm_sync_messages for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_attachments_reader_select
    on slack_dm_sync_message_attachments;
create policy centaur_slack_dm_attachments_reader_select
    on slack_dm_sync_message_attachments for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_dm_sync_message_attachments;
create policy centaur_readonly_slack_dm_sync_message_attachments_select
    on slack_dm_sync_message_attachments for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_checkpoints_reader_select
    on slack_dm_sync_checkpoints;
create policy centaur_slack_dm_checkpoints_reader_select
    on slack_dm_sync_checkpoints for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_dm_sync_checkpoints;
create policy centaur_readonly_slack_dm_sync_checkpoints_select
    on slack_dm_sync_checkpoints for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_context_documents_reader_select
    on slack_dm_context_documents;
create policy centaur_slack_dm_context_documents_reader_select
    on slack_dm_context_documents for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_context_documents_select
    on slack_dm_context_documents;
create policy centaur_readonly_slack_dm_context_documents_select
    on slack_dm_context_documents for select to centaur_readonly
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_slack_dm_conversation_context_documents_reader_select
    on slack_dm_conversation_context_documents;
create policy centaur_slack_dm_conversation_context_documents_reader_select
    on slack_dm_conversation_context_documents for select to centaur_slack_reader
    using (centaur_can_read_slack_user_conversation(home_team_id, conversation_id));

drop policy if exists centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_dm_conversation_context_documents;
create policy centaur_readonly_slack_dm_conversation_context_documents_select
    on slack_dm_conversation_context_documents for select to centaur_readonly
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
      from slack_dm_sync_conversations conversations
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
    on slack_dm_context_documents;
create trigger trg_label_slack_private_channel_message_document
    before insert or update on slack_dm_context_documents
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
      from slack_dm_sync_conversations conversations
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
    on slack_dm_conversation_context_documents;
create trigger trg_label_slack_private_channel_conversation_document
    before insert or update on slack_dm_conversation_context_documents
    for each row
    execute function centaur_label_slack_private_channel_conversation_document();
