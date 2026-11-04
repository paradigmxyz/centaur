alter table sessions
    add column if not exists sandbox_last_active_at timestamptz;

update sessions
set sandbox_last_active_at = coalesce(sandbox_last_active_at, updated_at, created_at)
where sandbox_id is not null;

create index if not exists sessions_sandbox_activity_idx
    on sessions (sandbox_last_active_at, thread_key)
    where sandbox_id is not null;

alter table session_warm_sandboxes
    drop constraint if exists session_warm_sandboxes_status_supported;

alter table session_warm_sandboxes
    add constraint session_warm_sandboxes_status_supported
        check (status in ('ready', 'claimed', 'evicting', 'failed'));

create index if not exists session_warm_sandboxes_evicting_idx
    on session_warm_sandboxes (updated_at, sandbox_id)
    where status = 'evicting';
