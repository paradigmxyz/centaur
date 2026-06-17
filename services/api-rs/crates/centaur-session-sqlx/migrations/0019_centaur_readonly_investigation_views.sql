create or replace view centaur_readonly_session_messages as
select
    message_id,
    thread_key,
    role,
    case
        when jsonb_typeof(parts) = 'array' then jsonb_array_length(parts)
        else 0
    end as part_count,
    coalesce(
        (
            select jsonb_agg(distinct coalesce(part_values.part ->> 'type', 'unknown'))
            from jsonb_array_elements(
                case
                    when jsonb_typeof(parts) = 'array' then parts
                    else '[]'::jsonb
                end
            ) as part_values(part)
        ),
        '[]'::jsonb
    ) as part_types,
    metadata ->> 'source' as source,
    metadata ->> 'platform' as platform,
    metadata ->> 'action' as action,
    metadata ->> 'user_id' as user_id,
    metadata ->> 'user_name' as user_name,
    created_at
from session_messages;

create or replace view centaur_readonly_session_events as
select
    event_id,
    thread_key,
    execution_id,
    event_type,
    payload ->> 'type' as payload_type,
    payload ->> 'subtype' as payload_subtype,
    payload ->> 'status' as status,
    payload ->> 'terminal_reason' as terminal_reason,
    payload ->> 'turn_id' as turn_id,
    payload ? 'error' as has_error,
    case
        when payload ? 'error' then octet_length(payload ->> 'error')
    end as error_length,
    coalesce(
        (
            select jsonb_agg(payload_keys.key)
            from jsonb_object_keys(payload) as payload_keys(key)
        ),
        '[]'::jsonb
    ) as payload_keys,
    created_at
from session_events;

do $$
begin
    if to_regclass('public.slack_sync_messages') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_sync_messages as
            select
                channel_id,
                message_ts,
                occurred_at,
                thread_ts,
                parent_message_ts,
                is_thread_root,
                user_id,
                bot_id <> '' as has_bot_id,
                message_type,
                message_subtype,
                permalink,
                reply_count,
                latest_reply_ts,
                thread_refreshed_at,
                source_run_id,
                first_seen_at,
                last_seen_at,
                updated_at
            from slack_sync_messages
        $view$;
    end if;

    if to_regclass('public.slack_sync_message_attachments') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_sync_message_attachments as
            select
                channel_id,
                message_ts,
                slack_file_id,
                name,
                title,
                mimetype,
                filetype,
                size_bytes,
                permalink,
                download_status,
                download_error <> '' as has_download_error,
                content_sha256 is not null as has_content_sha256,
                content_bytes is not null as has_content_bytes,
                source_run_id,
                first_seen_at,
                last_seen_at,
                updated_at
            from slack_sync_message_attachments
        $view$;
    end if;

    if to_regclass('public.slack_sync_backfill_jobs') is not null then
        execute $view$
            create or replace view centaur_readonly_slack_thread_backfill_jobs as
            select
                job_id,
                job_key,
                job_type,
                channel_id,
                payload_json ->> 'thread_ts' as thread_ts,
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
