do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_readonly') then
        create role centaur_readonly
            nologin
            nosuperuser
            nocreatedb
            nocreaterole
            noinherit
            noreplication;
    end if;
end
$$;

grant usage on schema public to centaur_readonly;

revoke select on all tables in schema public from centaur_readonly;
revoke usage, select on all sequences in schema public from centaur_readonly;

alter default privileges in schema public
    revoke select on tables from centaur_readonly;

alter default privileges in schema public
    revoke usage, select on sequences from centaur_readonly;

select absurd.create_queue('centaur_workflows');
select absurd.create_queue('centaur_workflows_etl');
select absurd.create_queue('centaur_workflow_schedules');

create or replace view centaur_readonly_sessions as
select
    thread_key,
    sandbox_id,
    harness_type,
    harness_thread_id,
    persona_id,
    status,
    metadata ->> 'source' as source,
    metadata ->> 'platform' as platform,
    metadata ->> 'thread_id' as external_thread_id,
    created_at,
    updated_at
from sessions;

create or replace view centaur_readonly_session_executions as
select
    execution_id,
    thread_key,
    status,
    to_jsonb(session_executions) ->> 'model' as model,
    to_jsonb(session_executions) ->> 'harness_run_id' as harness_run_id,
    to_jsonb(session_executions) ->> 'base_image_ref' as base_image_ref,
    to_jsonb(session_executions) ->> 'base_image_hash' as base_image_hash,
    to_jsonb(session_executions) ->> 'overlay_hash' as overlay_hash,
    metadata ->> 'source' as source,
    metadata ->> 'platform' as platform,
    metadata ->> 'action' as action,
    case
        when metadata ->> 'idle_timeout_ms' ~ '^[0-9]+$' then (metadata ->> 'idle_timeout_ms')::bigint
    end as idle_timeout_ms,
    case
        when metadata ->> 'max_duration_ms' ~ '^[0-9]+$' then (metadata ->> 'max_duration_ms')::bigint
    end as max_duration_ms,
    created_at,
    updated_at,
    started_at,
    completed_at,
    extract(epoch from completed_at - started_at) as duration_seconds
from session_executions;

create or replace view centaur_readonly_session_warm_sandboxes as
select
    sandbox_id,
    workload_key,
    status,
    claimed_thread_key,
    created_at,
    updated_at,
    claimed_at,
    last_error is not null as has_last_error
from session_warm_sandboxes;

create or replace view centaur_readonly_workflow_queues as
select
    queue_name,
    created_at,
    storage_mode,
    default_partition,
    cleanup_ttl,
    cleanup_limit
from absurd.queues;

create or replace view centaur_readonly_workflow_runs as
select
    'centaur_workflows'::text as queue_name,
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'workflow_name' as workflow_name,
    t.params ->> 'harness_type' as harness_type,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflows t
left join absurd.r_centaur_workflows r on r.run_id = t.last_attempt_run
union all
select
    'centaur_workflows_etl'::text as queue_name,
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'workflow_name' as workflow_name,
    t.params ->> 'harness_type' as harness_type,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflows_etl t
left join absurd.r_centaur_workflows_etl r on r.run_id = t.last_attempt_run;

create or replace view centaur_readonly_workflow_schedule_runs as
select
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'schedule_id' as schedule_id,
    nullif(t.params ->> 'scheduled_at', '')::timestamptz as scheduled_at,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflow_schedules t
left join absurd.r_centaur_workflow_schedules r on r.run_id = t.last_attempt_run;

do $$
begin
    if to_regclass('public.user_feedback') is not null then
        execute $view$
            create or replace view centaur_readonly_user_feedback as
            select
                feedback_id,
                source,
                user_id,
                channel_id,
                thread_ts,
                execution_id,
                message <> '' as has_message,
                octet_length(message) as message_length,
                created_at
            from user_feedback
        $view$;
    end if;

    if to_regclass('public.company_context_documents') is not null then
        execute $view$
            create or replace view centaur_readonly_company_context_document_stats as
            select
                source,
                source_type,
                access_scope,
                count(*) as row_count,
                count(*) filter (where parent_document_id is null) as root_document_count,
                count(*) filter (where parent_document_id is not null) as chunk_count,
                min(occurred_at) as earliest_occurred_at,
                max(occurred_at) as latest_occurred_at,
                max(source_updated_at) as latest_source_updated_at,
                max(updated_at) as latest_updated_at
            from company_context_documents
            group by source, source_type, access_scope
        $view$;
    end if;

    if to_regclass('public.slack_sync_channels') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_sync_channels as
            select
                channel_id,
                channel_name,
                is_archived,
                is_syncable,
                member_count,
                first_seen_at,
                last_seen_at,
                updated_at
            from slack_sync_channels
        $view$;
    end if;

    if to_regclass('public.slack_sync_users') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_sync_users as
            select
                user_id,
                user_name,
                real_name,
                display_name,
                is_bot,
                is_deleted,
                team_id,
                first_seen_at,
                last_seen_at,
                updated_at
            from slack_sync_users
        $view$;
    end if;

    if to_regclass('public.slack_sync_runs') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_sync_runs as
            select
                run_id,
                workflow_run_id,
                mode,
                status,
                channels_requested,
                channels_synced,
                channels_skipped,
                channels_failed,
                messages_fetched,
                messages_upserted,
                threads_fetched,
                replies_fetched,
                replies_upserted,
                started_at,
                finished_at,
                error_text is not null and error_text <> '' as has_error,
                metadata ->> 'source' as source
            from slack_sync_runs
        $view$;
    end if;

    if to_regclass('public.slack_sync_checkpoints') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_sync_checkpoints as
            select
                channel_id,
                watermark_ts,
                last_run_id,
                last_success_at,
                last_error <> '' as has_error,
                created_at,
                updated_at
            from slack_sync_checkpoints
        $view$;
    end if;

    if to_regclass('public.slack_sync_backfill_jobs') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_backfill_jobs as
            select
                job_id,
                job_type,
                channel_id,
                status,
                priority,
                attempt_count,
                last_run_id,
                last_enqueued_at,
                last_started_at,
                last_completed_at,
                last_error <> '' as has_error,
                created_at,
                updated_at
            from slack_sync_backfill_jobs
        $view$;
    end if;

    if to_regclass('public.google_drive_sync_runs') is not null then
        execute $view$
            create or replace view centaur_readonly_google_drive_sync_runs as
            select
                run_id,
                workflow_run_id,
                mode,
                status,
                scopes_requested,
                scopes_synced,
                scopes_failed,
                files_seen,
                files_upserted,
                docs_fetched,
                docs_upserted,
                started_at,
                finished_at,
                error_text <> '' as has_error
            from google_drive_sync_runs
        $view$;
    end if;

    if to_regclass('public.google_drive_sync_checkpoints') is not null then
        execute $view$
            create or replace view centaur_readonly_google_drive_sync_checkpoints as
            select
                scope_id,
                watermark_time,
                last_run_id,
                last_success_at,
                last_error <> '' as has_error,
                created_at,
                updated_at
            from google_drive_sync_checkpoints
        $view$;
    end if;

    if to_regclass('public.google_calendar_sync_runs') is not null then
        execute $view$
            create or replace view centaur_readonly_google_calendar_sync_runs as
            select
                run_id,
                workflow_run_id,
                mode,
                status,
                calendars_requested,
                calendars_synced,
                calendars_failed,
                calendars_seen,
                calendars_upserted,
                events_seen,
                events_upserted,
                events_cancelled,
                started_at,
                finished_at,
                error_text <> '' as has_error
            from google_calendar_sync_runs
        $view$;
    end if;

    if to_regclass('public.google_calendar_sync_checkpoints') is not null then
        execute $view$
            create or replace view centaur_readonly_google_calendar_sync_checkpoints as
            select
                calendar_id,
                watermark_time,
                last_run_id,
                last_success_at,
                last_error <> '' as has_error,
                created_at,
                updated_at
            from google_calendar_sync_checkpoints
        $view$;
    end if;

    if to_regclass('public.linear_sync_runs') is not null then
        execute $view$
            create or replace view centaur_readonly_linear_sync_runs as
            select
                run_id,
                workflow_run_id,
                mode,
                status,
                scopes_requested,
                scopes_synced,
                scopes_failed,
                projects_seen,
                projects_upserted,
                issues_seen,
                issues_upserted,
                comments_seen,
                comments_upserted,
                started_at,
                finished_at,
                error_text <> '' as has_error
            from linear_sync_runs
        $view$;
    end if;

    if to_regclass('public.linear_sync_checkpoints') is not null then
        execute $view$
            create or replace view centaur_readonly_linear_sync_checkpoints as
            select
                scope_id,
                watermark_time,
                last_run_id,
                last_success_at,
                last_error <> '' as has_error,
                created_at,
                updated_at
            from linear_sync_checkpoints
        $view$;
    end if;

    if to_regclass('public.workflow_runs') is not null then
        execute $view$
            create or replace view centaur_readonly_legacy_workflow_runs as
            select
                run_id,
                workflow_name,
                workflow_version,
                workflow_source_path,
                parent_run_id,
                root_run_id,
                thread_key,
                status,
                available_at,
                worker_id is not null as claimed,
                created_at,
                started_at,
                completed_at,
                updated_at
            from workflow_runs
        $view$;
    end if;

    if to_regclass('public.workflow_schedules') is not null then
        execute $view$
            create or replace view centaur_readonly_legacy_workflow_schedules as
            select
                schedule_id,
                workflow_name,
                schedule_kind,
                schedule_expr,
                timezone,
                interval_seconds,
                catchup_policy,
                enabled,
                next_run_at,
                last_run_at,
                created_at,
                updated_at
            from workflow_schedules
        $view$;
    end if;

    if to_regclass('public.workflow_checkpoints') is not null then
        execute $view$
            create or replace view centaur_readonly_legacy_workflow_checkpoints as
            select
                run_id,
                checkpoint_name,
                step_kind,
                execution_id,
                child_run_id,
                created_at
            from workflow_checkpoints
        $view$;
    end if;

    if to_regclass('public.workflow_events') is not null then
        execute $view$
            create or replace view centaur_readonly_legacy_workflow_events as
            select
                event_type,
                correlation_id,
                created_at
            from workflow_events
        $view$;
    end if;

    if to_regclass('public.agent_runtime_assignments') is not null then
        execute $view$
            create or replace view centaur_readonly_agent_runtime_assignments as
            select
                thread_key,
                assignment_generation,
                runtime_id,
                harness,
                engine,
                persona_id,
                prompt_ref,
                effective_agents_md_sha256,
                state,
                created_at,
                updated_at,
                released_at
            from agent_runtime_assignments
        $view$;
    end if;

    if to_regclass('public.agent_execution_requests') is not null then
        execute $view$
            create or replace view centaur_readonly_agent_execution_requests as
            select
                execution_id,
                thread_key,
                assignment_generation,
                execute_id,
                durable_turn_id,
                status,
                created_at,
                claimed_at,
                started_at,
                last_progress_at,
                silence_deadline_at,
                hard_deadline_at,
                stream_break_count,
                last_stream_break_at,
                completed_at,
                terminal_reason,
                worker_id is not null as claimed,
                updated_at
            from agent_execution_requests
        $view$;
    end if;

    if to_regclass('public.sandbox_sessions') is not null then
        execute $view$
            create or replace view centaur_readonly_sandbox_sessions as
            select
                thread_key,
                sandbox_id,
                harness,
                engine,
                state,
                last_delivered_id,
                agent_thread_id,
                inflight_turn_id,
                inflight_started_at,
                inflight_attempts,
                last_result_at,
                trace_id,
                started_at,
                updated_at,
                wire_connected_at,
                wire_last_seen_at
            from sandbox_sessions
        $view$;
    end if;

    if to_regclass('public.thread_traces') is not null then
        execute $view$
            create or replace view centaur_readonly_thread_traces as
            select
                thread_key,
                trace_id,
                root_span_id,
                created_at,
                updated_at
            from thread_traces
        $view$;
    end if;
end
$$;

do $$
declare
    relation_oid regclass;
begin
    for relation_oid in
        select c.oid::regclass
        from pg_class c
        join pg_namespace n on n.oid = c.relnamespace
        where n.nspname = 'public'
            and c.relkind in ('v', 'm')
            and c.relname like 'centaur_readonly\_%' escape '\'
    loop
        execute format('grant select on table %s to centaur_readonly', relation_oid);
    end loop;
end
$$;

grant centaur_readonly to current_user;
