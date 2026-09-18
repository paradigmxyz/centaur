grant select (
    channel_id,
    message_ts,
    occurred_at,
    thread_ts,
    parent_message_ts,
    is_thread_root,
    user_id,
    bot_id,
    message_type,
    message_subtype,
    text,
    permalink,
    reply_count,
    reply_users,
    latest_reply_ts,
    thread_refreshed_at,
    first_seen_at,
    last_seen_at,
    updated_at
) on slack_sync_messages to centaur_company_context_reader;

grant select (
    user_id,
    user_name,
    real_name,
    display_name,
    is_bot,
    is_deleted,
    team_id,
    first_seen_at,
    last_seen_at,
    updated_at
) on slack_sync_users to centaur_company_context_reader;

drop policy if exists centaur_cc_reader_messages_select
    on slack_sync_messages;
create policy centaur_cc_reader_messages_select
    on slack_sync_messages
    for select
    to centaur_company_context_reader
    using (
        exists (
            select 1
            from slack_sync_channels channels
            where channels.channel_id = slack_sync_messages.channel_id
        )
    );

drop policy if exists centaur_cc_reader_users_select
    on slack_sync_users;
create policy centaur_cc_reader_users_select
    on slack_sync_users
    for select
    to centaur_company_context_reader
    using (
        exists (
            select 1
            from slack_sync_messages messages
            where messages.user_id = slack_sync_users.user_id
        )
    );

create view company_context_slack_messages
with (security_invoker = true)
as
select
    channel_id,
    message_ts,
    occurred_at,
    thread_ts,
    parent_message_ts,
    is_thread_root,
    user_id,
    bot_id,
    message_type,
    message_subtype,
    text,
    permalink,
    reply_count,
    reply_users,
    latest_reply_ts,
    thread_refreshed_at,
    first_seen_at,
    last_seen_at,
    updated_at
from slack_sync_messages;

create view company_context_slack_users
with (security_invoker = true)
as
select
    user_id,
    user_name,
    real_name,
    display_name,
    is_bot,
    is_deleted,
    team_id,
    first_seen_at,
    last_seen_at,
    updated_at
from slack_sync_users;

grant select on
    company_context_slack_messages,
    company_context_slack_users
to centaur_company_context_reader;
