alter table session_executions
    add column if not exists blocking_reason text;

alter table session_executions
    add constraint session_executions_blocking_reason_supported
    check (
        blocking_reason is null
        or blocking_reason in ('awaiting_project_selection', 'workspace_provisioning')
    );

create table development_channel_bindings (
    binding_id text primary key,
    platform text not null,
    tenant_key text not null,
    conversation_key text not null,
    root_message_id text not null,
    session_generation integer not null default 1,
    thread_key text not null unique references sessions(thread_key) on delete cascade,
    initiator_principal_id text not null,
    active boolean not null default true,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint development_channel_bindings_generation_positive
        check (session_generation > 0)
);

create unique index development_channel_bindings_active_conversation_idx
    on development_channel_bindings
        (platform, tenant_key, conversation_key, root_message_id)
    where active;

create table development_platform_events (
    platform text not null,
    tenant_key text not null,
    event_id text not null,
    message_id text,
    thread_key text not null references sessions(thread_key) on delete cascade,
    created_at timestamptz not null default now(),
    primary key (platform, tenant_key, event_id)
);

create unique index development_platform_events_message_idx
    on development_platform_events (platform, tenant_key, message_id)
    where message_id is not null;

create table session_workspaces (
    workspace_id text primary key,
    thread_key text not null unique references sessions(thread_key) on delete cascade,
    state text not null,
    storage_ref text,
    preparation_attempt integer not null default 0,
    lease_owner text,
    lease_expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint session_workspaces_state_supported check (
        state in ('awaiting_selection', 'provisioning', 'ready', 'failed')
    ),
    constraint session_workspaces_preparation_attempt_nonnegative
        check (preparation_attempt >= 0),
    constraint session_workspaces_lease_complete check (
        (lease_owner is null) = (lease_expires_at is null)
    )
);

create table development_selection_flows (
    selection_flow_id text primary key,
    workspace_id text not null references session_workspaces(workspace_id) on delete cascade,
    execution_id text references session_executions(execution_id) on delete cascade,
    kind text not null,
    state text not null,
    version integer not null default 1,
    selected_repository_ids jsonb not null default '[]'::jsonb,
    decided_by_principal_id text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint development_selection_flows_kind_supported
        check (kind in ('initial', 'add')),
    constraint development_selection_flows_state_supported
        check (state in ('pending', 'confirmed', 'cancelled')),
    constraint development_selection_flows_version_positive check (version > 0),
    constraint development_selection_flows_selected_array
        check (jsonb_typeof(selected_repository_ids) = 'array')
);

create unique index development_selection_flows_one_pending_idx
    on development_selection_flows (workspace_id)
    where state = 'pending';

create table session_repositories (
    workspace_id text not null references session_workspaces(workspace_id) on delete cascade,
    repository_id text not null,
    gitlab_project_id bigint not null,
    display_name text not null,
    path_with_namespace text not null,
    default_branch text not null,
    clone_url text not null,
    relative_path text not null,
    base_sha text,
    local_branch text,
    head_sha text,
    selection_flow_id text references development_selection_flows(selection_flow_id),
    added_by_principal_id text not null,
    state text not null,
    provisioning_attempt integer not null default 0,
    failure_code text,
    failure_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (workspace_id, repository_id),
    unique (workspace_id, relative_path),
    constraint session_repositories_repository_id_shape
        check (repository_id = 'gitlab:' || gitlab_project_id::text),
    constraint session_repositories_project_id_positive check (gitlab_project_id > 0),
    constraint session_repositories_state_supported
        check (state in ('pending', 'provisioning', 'ready', 'failed')),
    constraint session_repositories_provisioning_attempt_nonnegative
        check (provisioning_attempt >= 0)
);

create table development_change_sets (
    changeset_id text primary key,
    workspace_id text not null references session_workspaces(workspace_id) on delete cascade,
    execution_id text not null unique references session_executions(execution_id) on delete cascade,
    initiator_principal_id text not null,
    state text not null,
    summary text,
    failure_code text,
    failure_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint development_change_sets_state_supported check (
        state in ('collecting', 'ready', 'needs_agent_completion', 'failed')
    )
);

create table development_change_set_repositories (
    changeset_repository_id text primary key,
    changeset_id text not null references development_change_sets(changeset_id) on delete cascade,
    workspace_id text not null,
    repository_id text not null,
    base_sha text not null,
    head_sha text not null,
    commit_metadata jsonb not null default '[]'::jsonb,
    changed_file_count integer not null,
    additions integer not null,
    deletions integer not null,
    patch_hash text not null,
    patch_artifact_ref text not null,
    test_evidence jsonb not null default '[]'::jsonb,
    created_at timestamptz not null default now(),
    unique (changeset_id, repository_id),
    foreign key (workspace_id, repository_id)
        references session_repositories(workspace_id, repository_id),
    constraint development_change_set_repositories_counts_nonnegative check (
        changed_file_count >= 0 and additions >= 0 and deletions >= 0
    )
);

create table development_publish_batches (
    publish_batch_id text primary key,
    changeset_id text not null unique
        references development_change_sets(changeset_id) on delete cascade,
    approver_principal_id text not null,
    idempotency_key text not null,
    state text not null,
    lease_owner text,
    lease_expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (changeset_id, idempotency_key),
    constraint development_publish_batches_state_supported check (
        state in ('pending', 'running', 'succeeded', 'partially_succeeded', 'failed')
    ),
    constraint development_publish_batches_lease_complete check (
        (lease_owner is null) = (lease_expires_at is null)
    )
);

create table development_publish_items (
    publish_item_id text primary key,
    publish_batch_id text not null
        references development_publish_batches(publish_batch_id) on delete cascade,
    changeset_repository_id text not null
        references development_change_set_repositories(changeset_repository_id),
    repository_id text not null,
    source_branch text not null,
    target_branch text not null,
    head_sha text not null,
    state text not null,
    attempt_count integer not null default 0,
    remote_branch_sha text,
    merge_request_iid bigint,
    merge_request_url text,
    failure_code text,
    failure_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (publish_batch_id, repository_id),
    constraint development_publish_items_state_supported check (
        state in ('pending', 'pushing', 'pushed', 'creating_mr', 'succeeded', 'failed')
    ),
    constraint development_publish_items_attempt_nonnegative check (attempt_count >= 0),
    constraint development_publish_items_mr_iid_positive
        check (merge_request_iid is null or merge_request_iid > 0)
);

create table feishu_deliveries (
    delivery_id text primary key,
    tenant_key text not null,
    thread_key text not null unique references sessions(thread_key) on delete cascade,
    chat_id text not null,
    root_message_id text not null,
    message_id text,
    card_id text,
    last_event_cursor bigint not null default 0,
    render_version integer not null default 0,
    state text not null default 'pending',
    failure_code text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint feishu_deliveries_cursor_nonnegative check (last_event_cursor >= 0),
    constraint feishu_deliveries_version_nonnegative check (render_version >= 0),
    constraint feishu_deliveries_state_supported
        check (state in ('pending', 'delivered', 'failed'))
);
