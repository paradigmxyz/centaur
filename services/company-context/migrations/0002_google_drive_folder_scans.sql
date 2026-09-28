-- Shared Drive folders shared with non-members have no change feed, so they are
-- re-walked each cycle. A walk records when it started; once its queue drains,
-- that credential's observations in the drive not seen since then are removed.
create table company_context_system.google_drive_folder_scans (
    scope_id text primary key,
    broker_credential_id bigint not null,
    drive_id text not null,
    started_at timestamptz not null default now(),
    -- Refreshed as the walk progresses; a stale heartbeat lets a new run take over.
    heartbeat_at timestamptz not null default now(),
    check (scope_id <> ''),
    check (drive_id <> '')
);

create index google_drive_folder_scans_credential_idx
    on company_context_system.google_drive_folder_scans (broker_credential_id);

create table company_context_system.google_drive_folder_scan_queue (
    scope_id text not null
        references company_context_system.google_drive_folder_scans(scope_id) on delete cascade,
    folder_id text not null,
    done boolean not null default false,
    -- A worker's lease on the folder; an expired lease returns it to the queue.
    claimed_until timestamptz,
    primary key (scope_id, folder_id),
    check (folder_id <> '')
);

create index google_drive_folder_scan_queue_pending_idx
    on company_context_system.google_drive_folder_scan_queue (scope_id, claimed_until)
    where not done;
