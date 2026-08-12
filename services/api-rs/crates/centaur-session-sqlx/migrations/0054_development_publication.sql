alter table session_workspaces
    drop constraint session_workspaces_state_supported;

alter table session_workspaces
    add constraint session_workspaces_state_supported check (
        state in (
            'awaiting_selection', 'provisioning', 'collecting',
            'publishing', 'ready', 'failed'
        )
    );

alter table session_workspaces
    add column workspace_revision bigint not null default 0,
    add constraint session_workspaces_revision_nonnegative check (workspace_revision >= 0);

alter table development_change_sets
    add column workspace_revision bigint not null default 0,
    add constraint development_change_sets_revision_nonnegative check (workspace_revision >= 0);

create table development_publish_requests (
    publish_request_id text primary key,
    publish_batch_id text not null
        references development_publish_batches(publish_batch_id) on delete cascade,
    request_kind text not null,
    requested_by_principal_id text not null,
    idempotency_key text not null,
    created_at timestamptz not null default now(),
    unique (publish_batch_id, idempotency_key),
    constraint development_publish_requests_kind_supported check (
        request_kind in ('approve', 'retry_failed')
    )
);

create index development_publish_batches_reconcile_idx
    on development_publish_batches (state, lease_expires_at, updated_at)
    where state in ('pending', 'running');
