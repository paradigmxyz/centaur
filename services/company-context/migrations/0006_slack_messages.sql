-- Slack message history synchronized through users' Slack broker credentials.
-- Messages are staged privately; reader access is added once they are
-- published for retrieval. Messages are removed with their conversation once
-- no live credential observes it.

alter table company_context_system.slack_conversations
    -- The span of history synchronized so far. Raising a conversation's
    -- history days moves its start back past history_synced_from.
    add column history_synced_from timestamptz,
    add column history_synced_until timestamptz,
    add column history_last_error text not null default '',
    -- The history sync task currently holding the conversation, so that syncs
    -- from consecutive discoveries do not page the same history concurrently.
    add column history_sync_task_id text,
    add column history_sync_heartbeat_at timestamptz;

create table company_context_system.slack_messages (
    conversation_id text not null
        references company_context_system.slack_conversations(conversation_id) on delete cascade,
    message_ts text not null,
    thread_ts text,
    user_id text not null default '',
    bot_id text not null default '',
    subtype text,
    text text not null default '',
    reply_count integer not null default 0,
    latest_reply_ts text,
    edited_ts text,
    occurred_at timestamptz not null,
    raw_payload jsonb not null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (conversation_id, message_ts),
    check (message_ts <> '')
);

create index slack_messages_thread_idx
    on company_context_system.slack_messages (conversation_id, thread_ts)
    where thread_ts is not null;

create index slack_messages_occurred_idx
    on company_context_system.slack_messages (occurred_at desc);
