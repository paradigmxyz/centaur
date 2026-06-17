"""Workflow: drain resumable Slack ETL backfill cursors without burdening incremental sync."""

from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from typing import Any

from api.vm_metrics import (
    record_etl_items_deleted,
    record_etl_items_enqueued,
    record_etl_items_failed,
    record_etl_items_seen,
    record_etl_items_upserted,
    set_etl_backfill_job_age_seconds,
    set_etl_backfill_jobs,
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import (
    BACKFILL_JOB_CHANNEL_BOOTSTRAP,
    BACKFILL_JOB_CHANNEL_CONTINUATION,
    BACKFILL_JOB_PAYLOAD_VERSION,
    BACKFILL_JOB_THREAD_REFRESH,
    channel_ref,
    claim_backfill_jobs,
    client as shared_client,
    enqueue_backfill_job,
    env_flag_enabled,
    failure_reason,
    is_permanent_slack_backfill_error,
    load_backfill_job_metrics,
    mark_thread_refreshed,
    mark_backfill_job_completed,
    mark_backfill_job_failed,
    mark_backfill_job_terminal_skipped,
    message_row,
    positive_int,
    record_run_finish,
    record_run_start,
    replace_thread_replies,
    upsert_messages,
    workflow_run_id_to_sync_run_id,
)

WORKFLOW_NAME = "slack_backfill"

DEFAULT_CHANNEL_PAGE_LIMIT = 200
DEFAULT_THREAD_REPLY_PAGE_LIMIT = 200
DEFAULT_SYNC_INTERVAL_SECONDS = 10 * 60
DEFAULT_CHANNEL_BATCH_LIMIT = positive_int(
    os.getenv("SLACK_BACKFILL_CHANNEL_BATCH_LIMIT"),
    50,
)
DEFAULT_CHANNEL_PAGES_PER_JOB = positive_int(
    os.getenv("SLACK_BACKFILL_CHANNEL_PAGES_PER_JOB"),
    5,
)

SCHEDULE = {
    "schedule_id": "slack_backfill",
    "interval_seconds": positive_int(
        os.getenv("SLACK_BACKFILL_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": (
        env_flag_enabled("SLACK_ETL_ENABLED", default=False)
        and env_flag_enabled("SLACK_BACKFILL_ENABLED", default=True)
    ),
    "no_delivery": True,
}


async def _emit_backfill_job_metrics(pool) -> None:
    """Publish current Slack backfill queue state for Grafana panels."""
    for row in await load_backfill_job_metrics(pool):
        job_type = str(row["job_type"])
        status = str(row["status"])
        set_etl_backfill_jobs("slack", job_type, status, int(row["job_count"]))
        set_etl_backfill_job_age_seconds(
            "slack",
            job_type,
            status,
            float(row["oldest_age_seconds"]),
        )


@dataclass
class Input:
    """Runtime options for Slack historical backfill draining."""

    limit: int = DEFAULT_CHANNEL_PAGE_LIMIT
    thread_reply_limit: int = DEFAULT_THREAD_REPLY_PAGE_LIMIT
    channel_batch_limit: int = DEFAULT_CHANNEL_BATCH_LIMIT
    channel_pages_per_job: int = DEFAULT_CHANNEL_PAGES_PER_JOB
    metadata: dict[str, Any] = field(default_factory=dict)


def _channel_job_payload(job: dict[str, Any]) -> dict[str, Any]:
    """Validate and extract a typed channel-history backfill payload."""
    if str(job.get("job_type") or "") not in {
        BACKFILL_JOB_CHANNEL_BOOTSTRAP,
        BACKFILL_JOB_CHANNEL_CONTINUATION,
    }:
        raise RuntimeError(f"unsupported backfill job type: {job.get('job_type')}")
    if int(job.get("payload_version") or 0) != BACKFILL_JOB_PAYLOAD_VERSION:
        raise RuntimeError(
            f"unsupported payload version for {job.get('job_key')}: {job.get('payload_version')}"
        )
    payload = job.get("payload_json")
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"invalid payload for {job.get('job_key')}") from exc
    if not isinstance(payload, dict):
        raise RuntimeError(f"invalid payload for {job.get('job_key')}")
    return payload


def _job_state(job: dict[str, Any], payload: dict[str, Any]) -> dict[str, Any]:
    """Translate a queued backfill payload into Slack client continuation state."""
    if str(job.get("job_type") or "") == BACKFILL_JOB_CHANNEL_BOOTSTRAP:
        return {
            "cursor": str(payload.get("cursor") or "") or None,
            "oldest": str(payload.get("window_oldest") or "") or None,
            "latest": str(payload.get("window_latest") or "") or None,
        }
    return {
        "cursor": str(payload.get("cursor") or "") or None,
        "oldest": str(payload.get("oldest") or "") or None,
        "latest": str(payload.get("latest") or "") or None,
    }


def _next_channel_payload(
    job: dict[str, Any],
    payload: dict[str, Any],
    next_state: dict[str, Any],
) -> dict[str, Any]:
    """Return the in-row payload to resume a channel-history backfill later."""
    if str(job.get("job_type") or "") == BACKFILL_JOB_CHANNEL_BOOTSTRAP:
        return {
            "cursor": next_state.get("cursor"),
            "window_oldest": payload.get("window_oldest"),
            "window_latest": payload.get("window_latest"),
            "lookback_days": int(payload.get("lookback_days") or 0),
            "thread_lookback_days": int(payload.get("thread_lookback_days") or 0),
        }
    return {
        "cursor": next_state.get("cursor"),
        "oldest": next_state.get("oldest"),
        "latest": next_state.get("latest"),
        "lookback_days": int(payload.get("lookback_days") or 0),
        "thread_lookback_days": int(payload.get("thread_lookback_days") or 0),
    }


def _thread_refresh_job_key(channel_id: str, thread_ts: str) -> str:
    """Return the stable job key for refreshing one thread's reply set."""
    return f"thread_refresh:{channel_id}:{thread_ts}"


def _thread_refresh_payload(job: dict[str, Any]) -> dict[str, Any]:
    """Validate and extract a typed thread refresh payload."""
    if str(job.get("job_type") or "") != BACKFILL_JOB_THREAD_REFRESH:
        raise RuntimeError(f"unsupported backfill job type: {job.get('job_type')}")
    if int(job.get("payload_version") or 0) != BACKFILL_JOB_PAYLOAD_VERSION:
        raise RuntimeError(
            f"unsupported payload version for {job.get('job_key')}: {job.get('payload_version')}"
        )
    payload = job.get("payload_json")
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"invalid payload for {job.get('job_key')}") from exc
    if not isinstance(payload, dict) or not str(payload.get("thread_ts") or ""):
        raise RuntimeError(f"invalid payload for {job.get('job_key')}")
    return payload


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Drain queued Slack backfill continuations in small, bounded batches."""
    if not (
        env_flag_enabled("SLACK_ETL_ENABLED", default=False)
        and env_flag_enabled("SLACK_BACKFILL_ENABLED", default=True)
    ):
        ctx.log("slack_backfill_skipped_disabled")
        return {
            "status": "skipped",
            "reason": "slack_backfill_disabled",
        }

    limit = positive_int(inp.limit, DEFAULT_CHANNEL_PAGE_LIMIT)
    thread_reply_limit = positive_int(
        inp.thread_reply_limit, DEFAULT_THREAD_REPLY_PAGE_LIMIT
    )
    channel_batch_limit = positive_int(
        inp.channel_batch_limit, DEFAULT_CHANNEL_BATCH_LIMIT
    )
    channel_pages_per_job = positive_int(
        inp.channel_pages_per_job, DEFAULT_CHANNEL_PAGES_PER_JOB
    )
    await _emit_backfill_job_metrics(ctx._pool)
    jobs = await claim_backfill_jobs(ctx._pool, channel_batch_limit)
    if not jobs:
        ctx.log("slack_backfill_skipped_no_jobs")
        return {
            "status": "skipped",
            "reason": "no_pending_backfills",
        }
    await _emit_backfill_job_metrics(ctx._pool)

    client = shared_client()
    access_mode = client._etl_access_mode()
    run_id = workflow_run_id_to_sync_run_id(ctx.run_id)
    requested = [
        {
            "channel_id": str(job["channel_id"]),
            "channel_name": "",
            "reason": str(job["job_key"]),
        }
        for job in jobs
    ]
    await record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        mode="backfill",
        requested=requested,
        skipped=[],
        metadata={
            **inp.metadata,
            "slack_access_mode": access_mode,
            "backfill_channel_batch_limit": channel_batch_limit,
            "backfill_channel_pages_per_job": channel_pages_per_job,
        },
    )

    synced: list[dict[str, str]] = []
    skipped: list[dict[str, str]] = []
    failed: list[dict[str, str]] = []
    counts = {
        "messages_fetched": 0,
        "messages_upserted": 0,
        "threads_fetched": 0,
        "replies_fetched": 0,
        "replies_upserted": 0,
    }

    for job in jobs:
        job_id = int(job["job_id"])
        channel_id = str(job["channel_id"] or "")
        try:
            if str(job.get("job_type") or "") == BACKFILL_JOB_THREAD_REFRESH:
                payload = _thread_refresh_payload(job)
                thread_ts = str(payload["thread_ts"])
                reply_cursor = None
                seen_reply_cursors: set[str] = set()
                all_reply_rows: list[dict[str, Any]] = []
                counts["threads_fetched"] += 1
                while True:
                    replies_page = client._get_etl_thread_replies_page(
                        channel_id,
                        thread_ts=thread_ts,
                        limit=thread_reply_limit,
                        cursor=reply_cursor,
                        inclusive=True,
                    )
                    replies = [
                        reply
                        for reply in replies_page.get("messages", [])
                        if str(reply.get("timestamp") or "") != thread_ts
                    ]
                    reply_rows = [
                        message_row(reply, run_id, thread_ts) for reply in replies
                    ]
                    all_reply_rows.extend(reply_rows)
                    counts["replies_fetched"] += len(reply_rows)
                    record_etl_items_seen(
                        "slack",
                        "channel",
                        "thread_refresh_reply",
                        len(reply_rows),
                    )
                    next_reply_cursor = replies_page.get("next_cursor")
                    if not replies_page.get("has_more") or not next_reply_cursor:
                        break
                    if next_reply_cursor in seen_reply_cursors:
                        raise RuntimeError(
                            f"Slack returned a repeated reply cursor for thread {thread_ts}"
                        )
                    seen_reply_cursors.add(next_reply_cursor)
                    reply_cursor = str(next_reply_cursor)

                replies_upserted, replies_deleted = await replace_thread_replies(
                    ctx._pool,
                    channel_id=channel_id,
                    thread_ts=thread_ts,
                    reply_rows=all_reply_rows,
                )
                counts["replies_upserted"] += replies_upserted
                record_etl_items_upserted(
                    "slack",
                    "channel",
                    "thread_refresh_reply",
                    replies_upserted,
                )
                record_etl_items_deleted(
                    "slack",
                    "channel",
                    "thread_refresh_reply",
                    replies_deleted,
                )
                await mark_thread_refreshed(
                    ctx._pool,
                    channel_id=channel_id,
                    thread_ts=thread_ts,
                )
                await mark_backfill_job_completed(
                    ctx._pool, job_id=job_id, run_id=run_id
                )
                synced.append(channel_ref({"id": channel_id, "name": channel_id}))
                ctx.log(
                    "slack_backfill_thread_refresh_completed",
                    job_id=job_id,
                    job_key=str(job["job_key"]),
                    job_type=str(job["job_type"]),
                    channel_id=channel_id,
                    thread_ts=thread_ts,
                    replies=len(all_reply_rows),
                    replies_upserted=replies_upserted,
                    replies_deleted=replies_deleted,
                )
                continue

            payload = _channel_job_payload(job)
            next_state: dict[str, Any] = {}
            page_count = 0
            message_count = 0
            thread_count = 0
            while True:
                page_count += 1
                page = client._sync_etl_channel_history(
                    channel_id,
                    state=_job_state(job, payload),
                    limit=limit,
                    lookback_days=int(payload.get("lookback_days") or 0),
                )
                messages = page.get("messages") or []
                message_count += len(messages)
                message_rows = [message_row(msg, run_id) for msg in messages]
                counts["messages_fetched"] += len(message_rows)
                record_etl_items_seen(
                    "slack",
                    "channel",
                    "backfill_root_message",
                    len(message_rows),
                )
                messages_upserted = await upsert_messages(ctx._pool, message_rows)
                counts["messages_upserted"] += messages_upserted
                record_etl_items_upserted(
                    "slack",
                    "channel",
                    "backfill_root_message",
                    messages_upserted,
                )

                thread_roots = {
                    str(msg.get("timestamp"))
                    for msg in messages
                    if msg.get("timestamp") and int(msg.get("reply_count") or 0) > 0
                }
                for thread_ts in sorted(thread_roots):
                    thread_count += 1
                    counts["threads_fetched"] += 1
                    await enqueue_backfill_job(
                        ctx._pool,
                        job_key=_thread_refresh_job_key(channel_id, thread_ts),
                        job_type=BACKFILL_JOB_THREAD_REFRESH,
                        channel_id=channel_id,
                        payload={"thread_ts": thread_ts},
                        run_id=run_id,
                        priority=200,
                        refresh_completed=False,
                    )
                    record_etl_items_enqueued(
                        "slack", "channel", "thread_refresh_job", 1
                    )

                next_state = page.get("sync_state") or {}
                if not next_state.get("cursor") or page_count >= channel_pages_per_job:
                    break
                payload = _next_channel_payload(job, payload, next_state)

            if next_state.get("cursor"):
                await enqueue_backfill_job(
                    ctx._pool,
                    job_key=str(job["job_key"]),
                    job_type=str(job["job_type"]),
                    channel_id=channel_id,
                    payload=_next_channel_payload(job, payload, next_state),
                    run_id=run_id,
                    priority=int(job.get("priority") or 100),
                )
                record_etl_items_enqueued(
                    "slack",
                    "channel",
                    f"{str(job['job_type'])}_job",
                    1,
                )
            else:
                await mark_backfill_job_completed(
                    ctx._pool,
                    job_id=job_id,
                    run_id=run_id,
                    payload=_next_channel_payload(job, payload, next_state),
                )

            synced.append(channel_ref({"id": channel_id, "name": channel_id}))
            ctx.log(
                "slack_backfill_channel_completed",
                job_id=job_id,
                job_key=str(job["job_key"]),
                job_type=str(job["job_type"]),
                channel_id=channel_id,
                pages=page_count,
                messages=message_count,
                threads=thread_count,
                has_more=bool(next_state.get("cursor")),
            )
        except Exception as exc:
            error = str(exc)
            ctx.log(
                "slack_backfill_channel_failed",
                job_id=job_id,
                job_key=str(job["job_key"]),
                job_type=str(job.get("job_type") or ""),
                channel_id=channel_id,
                error=error,
            )
            if is_permanent_slack_backfill_error(error):
                skipped.append(
                    channel_ref({"id": channel_id, "name": channel_id}, error)
                )
                ctx.log(
                    "slack_backfill_job_terminal_skipped",
                    job_id=job_id,
                    job_key=str(job["job_key"]),
                    job_type=str(job.get("job_type") or ""),
                    channel_id=channel_id,
                    error=error,
                )
                await mark_backfill_job_terminal_skipped(
                    ctx._pool,
                    job_id=job_id,
                    run_id=run_id,
                    error=error,
                )
                continue
            failed.append(channel_ref({"id": channel_id, "name": channel_id}, error))
            record_etl_items_failed(
                "slack",
                "channel",
                f"{str(job.get('job_type') or 'backfill')}_job",
                failure_reason(error),
            )
            await mark_backfill_job_failed(
                ctx._pool,
                job_id=job_id,
                run_id=run_id,
                error=error,
            )

    status = "completed"
    error_text = ""
    if failed and synced:
        status = "partial_failed"
        error_text = f"{len(failed)} channel(s) failed"
    elif failed:
        status = "failed"
        error_text = f"{len(failed)} channel(s) failed"

    await record_run_finish(
        ctx._pool,
        run_id=run_id,
        status=status,
        synced=synced,
        skipped=skipped,
        failed=failed,
        counts=counts,
        error_text=error_text,
    )
    await _emit_backfill_job_metrics(ctx._pool)

    return {
        "status": status,
        "run_id": run_id,
        "channels_synced": len(synced),
        "channels_skipped": len(skipped),
        "channels_failed": len(failed),
        **counts,
    }
