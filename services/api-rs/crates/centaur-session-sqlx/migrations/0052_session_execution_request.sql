alter table session_executions
    add column if not exists request jsonb not null default '{}'::jsonb;
