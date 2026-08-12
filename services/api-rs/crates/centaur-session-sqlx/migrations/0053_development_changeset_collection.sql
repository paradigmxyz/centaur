alter table session_workspaces
    drop constraint session_workspaces_state_supported;

alter table session_workspaces
    add constraint session_workspaces_state_supported check (
        state in ('awaiting_selection', 'provisioning', 'collecting', 'ready', 'failed')
    );

alter table development_change_sets
    add column lease_owner text,
    add column lease_expires_at timestamptz,
    add constraint development_change_sets_lease_complete check (
        (lease_owner is null) = (lease_expires_at is null)
    );

alter table development_change_set_repositories
    add column state text not null default 'changed',
    add column recorded_head_sha text,
    add column failure_code text,
    add column failure_message text,
    add constraint development_change_set_repositories_state_supported check (
        state in ('changed', 'needs_agent_completion', 'failed')
    );

alter table development_change_set_repositories
    alter column head_sha drop not null,
    alter column patch_hash drop not null,
    alter column patch_artifact_ref drop not null;

update development_change_set_repositories
   set recorded_head_sha = base_sha
 where recorded_head_sha is null;

alter table development_change_set_repositories
    alter column recorded_head_sha set not null;

create table development_artifacts (
    artifact_ref text primary key,
    sha256 text not null unique,
    media_type text not null,
    byte_length integer not null,
    content bytea not null,
    created_at timestamptz not null default now(),
    constraint development_artifacts_size_nonnegative check (byte_length >= 0),
    constraint development_artifacts_size_matches check (byte_length = octet_length(content))
);

alter table development_change_set_repositories
    add constraint development_change_set_repositories_artifact_ref_fkey
    foreign key (patch_artifact_ref) references development_artifacts(artifact_ref);
