alter table session_messages
    add column if not exists client_message_id text;

create unique index if not exists session_messages_thread_client_message_idx
    on session_messages (thread_key, client_message_id)
    where client_message_id is not null;

alter table session_executions
    add column if not exists idempotency_key text;

create unique index if not exists session_executions_thread_idempotency_idx
    on session_executions (thread_key, idempotency_key)
    where idempotency_key is not null;
