-- Shared Drive folders shared with non-members have no change feed, so they are
-- re-walked each cycle. Absurd runs the walk as one task per folder batch; these
-- tables only record which batches a walk has spawned, so that its cleanup runs
-- exactly once, after every batch has finished.
create table company_context_system.google_drive_folder_walks (
    scope_id text primary key,
    broker_credential_id bigint not null,
    drive_id text not null,
    -- Identifies the walk; files not seen since it began are removed at the end.
    started_at timestamptz not null default now(),
    check (scope_id <> ''),
    check (drive_id <> '')
);

create index google_drive_folder_walks_credential_idx
    on company_context_system.google_drive_folder_walks (broker_credential_id);

-- A batch row is inserted in the same transaction that spawns its task, and
-- marked done in the transaction that spawns its children.
create table company_context_system.google_drive_folder_walk_batches (
    scope_id text not null
        references company_context_system.google_drive_folder_walks(scope_id) on delete cascade,
    batch_id uuid not null,
    done boolean not null default false,
    primary key (scope_id, batch_id)
);

create index google_drive_folder_walk_batches_pending_idx
    on company_context_system.google_drive_folder_walk_batches (scope_id)
    where not done;
