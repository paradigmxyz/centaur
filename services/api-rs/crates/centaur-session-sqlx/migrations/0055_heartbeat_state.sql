create table if not exists heartbeat_profiles (
    profile_id uuid primary key,
    namespace text not null,
    name text not null,
    scope_kind text not null,
    scope_ref text not null,
    workflow_name text not null,
    executor_principal_foreign_id text not null,
    definition_hash text not null,
    definition_version integer not null,
    destination jsonb not null default '{}'::jsonb,
    required_sources text[] not null default '{}',
    optional_sources text[] not null default '{}',
    delivery_policy jsonb not null default '{}'::jsonb,
    enabled boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint heartbeat_profiles_scope_kind_check
        check (scope_kind in ('organization', 'team', 'personal')),
    constraint heartbeat_profiles_definition_version_check
        check (definition_version > 0),
    constraint heartbeat_profiles_name_len
        check (octet_length(name) between 1 and 128),
    unique (namespace, name)
);

create table if not exists heartbeat_profile_grants (
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    subject_kind text not null,
    subject_ref text not null,
    permission text not null,
    granted_by_principal text not null,
    created_at timestamptz not null default now(),
    constraint heartbeat_profile_grants_subject_kind_check
        check (subject_kind in ('principal', 'role', 'namespace_member')),
    constraint heartbeat_profile_grants_permission_check
        check (permission in ('view', 'review', 'admin', 'operate')),
    primary key (profile_id, subject_kind, subject_ref, permission)
);

create table if not exists heartbeat_runs (
    run_id uuid primary key,
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    workflow_run_id uuid not null,
    workflow_task_id uuid,
    trigger text not null,
    scheduled_for timestamptz,
    profile_definition_hash text not null,
    prompt_version text not null,
    executor_principal_foreign_id text not null,
    status text not null,
    outcome text,
    source_health jsonb not null default '{}'::jsonb,
    candidate_count integer not null default 0,
    surfaced_count integer not null default 0,
    memory_proposal_count integer not null default 0,
    error jsonb,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    constraint heartbeat_runs_trigger_check
        check (trigger in ('schedule', 'manual', 'event', 'replay')),
    constraint heartbeat_runs_status_check
        check (status in ('collecting', 'synthesizing', 'committing', 'delivering',
                          'completed', 'partial', 'failed', 'cancelled')),
    constraint heartbeat_runs_outcome_check
        check (outcome is null or outcome in ('attention', 'clean', 'degraded', 'none')),
    constraint heartbeat_runs_counts_check
        check (candidate_count >= 0 and surfaced_count >= 0 and memory_proposal_count >= 0),
    unique (profile_id, workflow_run_id)
);

create index if not exists heartbeat_runs_profile_started_idx
    on heartbeat_runs (profile_id, started_at desc, run_id);

create table if not exists heartbeat_source_checkpoints (
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    source_key text not null,
    cursor jsonb,
    watermark timestamptz,
    last_attempted_at timestamptz,
    last_succeeded_at timestamptz,
    last_complete_scan_at timestamptz,
    freshness_deadline timestamptz,
    consecutive_failures integer not null default 0,
    last_error jsonb,
    version integer not null default 0,
    constraint heartbeat_source_checkpoints_failures_check
        check (consecutive_failures >= 0),
    constraint heartbeat_source_checkpoints_version_check
        check (version >= 0),
    primary key (profile_id, source_key)
);

create table if not exists heartbeat_observations (
    observation_id uuid primary key,
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    run_id uuid references heartbeat_runs(run_id) on delete set null,
    source_key text not null,
    source_object_id text not null,
    source_revision text not null,
    source_updated_at timestamptz,
    captured_at timestamptz not null default now(),
    content_hash text not null,
    entity_keys text[] not null default '{}',
    title text not null,
    source_url text,
    normalized_payload jsonb not null,
    sensitivity text not null default 'internal',
    constraint heartbeat_observations_sensitivity_check
        check (sensitivity in ('public', 'internal', 'confidential', 'restricted')),
    unique (profile_id, source_key, source_object_id, source_revision)
);

create index if not exists heartbeat_observations_source_object_idx
    on heartbeat_observations (profile_id, source_key, source_object_id, captured_at desc);

create index if not exists heartbeat_observations_entity_keys_idx
    on heartbeat_observations using gin (entity_keys);

create table if not exists heartbeat_items (
    item_id uuid primary key,
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    story_key text not null,
    item_type text not null,
    entity_keys text[] not null default '{}',
    title text not null,
    summary text,
    status text not null default 'open',
    disposition text,
    priority_tier integer not null default 3,
    due_at timestamptz,
    owner_ref text,
    proposed_action jsonb,
    material_hash text not null,
    first_seen_at timestamptz not null default now(),
    last_changed_at timestamptz not null default now(),
    last_surfaced_at timestamptz,
    snooze_until timestamptz,
    resolved_at timestamptz,
    version integer not null default 1,
    constraint heartbeat_items_status_check
        check (status in ('open', 'snoozed', 'resolved', 'dismissed', 'stale')),
    constraint heartbeat_items_priority_check
        check (priority_tier between 0 and 9),
    constraint heartbeat_items_version_check
        check (version > 0),
    unique (profile_id, story_key)
);

create index if not exists heartbeat_items_candidates_idx
    on heartbeat_items (profile_id, status, priority_tier, due_at, last_changed_at desc);

create table if not exists heartbeat_item_observations (
    item_id uuid not null references heartbeat_items(item_id) on delete cascade,
    observation_id uuid not null references heartbeat_observations(observation_id) on delete cascade,
    relation text not null,
    linked_by text not null,
    created_at timestamptz not null default now(),
    constraint heartbeat_item_observations_relation_check
        check (relation in ('primary', 'supports', 'contradicts', 'context')),
    constraint heartbeat_item_observations_linked_by_check
        check (linked_by in ('deterministic', 'model_proposed', 'human')),
    primary key (item_id, observation_id)
);

create table if not exists heartbeat_item_events (
    event_id uuid primary key,
    item_id uuid not null references heartbeat_items(item_id) on delete cascade,
    run_id uuid references heartbeat_runs(run_id) on delete set null,
    event_type text not null,
    from_status text,
    to_status text,
    item_version integer not null,
    actor_kind text not null,
    actor_ref text,
    reason text,
    payload jsonb not null default '{}'::jsonb,
    idempotency_key text not null unique,
    created_at timestamptz not null default now(),
    constraint heartbeat_item_events_type_check
        check (event_type in ('created', 'material_change', 'unsnoozed', 'synthesized', 'surfaced',
                              'approve', 'assign', 'park', 'snooze', 'not_useful',
                              'prepare_draft')),
    constraint heartbeat_item_events_actor_kind_check
        check (actor_kind in ('system', 'model', 'human', 'source'))
);

create index if not exists heartbeat_item_events_item_created_idx
    on heartbeat_item_events (item_id, created_at, event_id);

create table if not exists heartbeat_run_artifacts (
    artifact_id uuid primary key,
    run_id uuid not null references heartbeat_runs(run_id) on delete cascade,
    artifact_kind text not null,
    artifact_key text not null,
    content jsonb not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    constraint heartbeat_run_artifacts_kind_check
        check (artifact_kind in ('source_input', 'source_error', 'ranked_candidates',
                                 'synthesis_output', 'delivery_preview')),
    unique (run_id, artifact_kind, artifact_key)
);

create index if not exists heartbeat_run_artifacts_run_created_idx
    on heartbeat_run_artifacts (run_id, created_at, artifact_id);

create table if not exists heartbeat_deliveries (
    delivery_id uuid primary key,
    run_id uuid not null references heartbeat_runs(run_id) on delete cascade,
    destination_kind text not null,
    destination_ref text not null,
    status text not null,
    client_message_id text not null unique,
    provider_message_id text,
    rendered_payload jsonb not null,
    error jsonb,
    created_at timestamptz not null default now(),
    sent_at timestamptz,
    constraint heartbeat_deliveries_status_check
        check (status in ('pending', 'sent', 'failed', 'superseded'))
);

create table if not exists heartbeat_action_tokens (
    token_hash text primary key,
    delivery_id uuid not null references heartbeat_deliveries(delivery_id) on delete cascade,
    item_id uuid not null references heartbeat_items(item_id) on delete cascade,
    item_version integer not null,
    action text not null,
    payload jsonb not null default '{}'::jsonb,
    expires_at timestamptz not null,
    consumed_at timestamptz,
    consumed_by_principal text,
    created_at timestamptz not null default now(),
    constraint heartbeat_action_tokens_action_check
        check (action in ('approve', 'assign', 'park', 'snooze', 'not_useful', 'prepare_draft')),
    constraint heartbeat_action_tokens_item_version_check
        check (item_version > 0),
    constraint heartbeat_action_tokens_consumed_check
        check ((consumed_at is null) = (consumed_by_principal is null))
);

revoke all on heartbeat_profiles, heartbeat_profile_grants, heartbeat_runs,
    heartbeat_source_checkpoints, heartbeat_observations, heartbeat_items,
    heartbeat_item_observations, heartbeat_item_events, heartbeat_run_artifacts,
    heartbeat_deliveries, heartbeat_action_tokens from public;
