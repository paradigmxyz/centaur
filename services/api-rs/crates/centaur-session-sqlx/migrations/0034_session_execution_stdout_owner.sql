alter table session_executions
    add column if not exists stdout_owner_id text,
    add column if not exists stdout_owner_lease_expires_at timestamptz;

create index if not exists session_executions_stdout_owner_lease_idx
    on session_executions (stdout_owner_lease_expires_at)
    where status in ('queued', 'running') and stdout_owner_id is not null;
