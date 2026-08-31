-- Keep the original migration immutable while allowing retry-attempt records
-- on upgraded databases.
alter table public.heartbeat_run_artifacts
    drop constraint if exists heartbeat_run_artifacts_kind_check;
alter table public.heartbeat_run_artifacts
    add constraint heartbeat_run_artifacts_kind_check
    check (artifact_kind in ('source_input', 'source_error', 'ranked_candidates',
                             'synthesis_output', 'delivery_preview',
                             'synthesis_attempt'));
