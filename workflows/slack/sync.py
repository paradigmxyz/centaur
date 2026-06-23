"""Workflow: sync recent public Slack channel history into Postgres."""

from __future__ import annotations

import datetime as dt
import fnmatch
import os
from dataclasses import dataclass, field
from typing import Any

from api.runtime_control import canonical_json
from api.vm_metrics import (
    record_etl_items_enqueued,
    record_etl_items_failed,
    record_etl_items_seen,
    record_etl_items_upserted,
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import (
    BACKFILL_JOB_CHANNEL_BOOTSTRAP,
    BACKFILL_JOB_CHANNEL_CONTINUATION,
    BACKFILL_JOB_THREAD_REFRESH,
    channel_ref,
    client as shared_client,
    emit_slack_checkpoint_metrics,
    enqueue_backfill_job,
    env_flag_enabled,
    failure_reason,
    load_thread_refresh_times,
    message_row,
    positive_int,
    record_run_finish,
    record_run_start,
    upsert_messages,
    widen_channel_bootstrap_job,
    workflow_run_id_to_sync_run_id,
)

WORKFLOW_NAME = "slack_sync"

DEFAULT_LOOKBACK_DAYS = 30
DEFAULT_THREAD_LOOKBACK_DAYS = 3
DEFAULT_THREAD_REFRESH_INTERVAL_HOURS = 12
DEFAULT_CHANNEL_PAGE_LIMIT = 100
DEFAULT_THREAD_REPLY_PAGE_LIMIT = 200
DEFAULT_SYNC_INTERVAL_SECONDS = 3_600
EXCLUDED_CHANNELS_ENV = "SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS"


def _env_flag_enabled(name: str, default: bool = False) -> bool:
    """Read a boolean feature flag where common false strings opt out."""
    return env_flag_enabled(name, default=default)


def _channel_exclusion_patterns(value: str | None) -> list[str]:
    """Parse comma-separated Slack channel exclusion globs."""
    if not value:
        return []
    patterns = []
    for raw_pattern in value.split(","):
        pattern = raw_pattern.strip().lower().lstrip("#")
        if pattern:
            patterns.append(pattern)
    return patterns


def _channel_name(channel: dict[str, Any]) -> str:
    """Return the normalized channel name used for config matching."""
    return str(channel.get("name") or "").strip().lower().lstrip("#")


def _channel_exclusion_reason(
    channel: dict[str, Any], patterns: list[str]
) -> str | None:
    """Return the configured pattern excluding a channel, if any."""
    name = _channel_name(channel)
    if not name:
        return None
    for pattern in patterns:
        if fnmatch.fnmatchcase(name, pattern):
            return f"excluded_by_config:{pattern}"
    return None


def _filter_excluded_channels(
    channels: list[dict[str, Any]],
    patterns: list[str],
) -> tuple[list[dict[str, Any]], list[dict[str, str]]]:
    """Split Slack channels into included channels and configured exclusions."""
    included = []
    excluded = []
    for channel in channels:
        reason = _channel_exclusion_reason(channel, patterns)
        if reason:
            excluded.append(channel_ref(channel, reason))
        else:
            included.append(channel)
    return included, excluded


SCHEDULE = {
    "schedule_id": "slack_sync",
    "interval_seconds": positive_int(
        os.getenv("SLACK_SYNC_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": _env_flag_enabled("SLACK_ETL_ENABLED"),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for a manual Slack sync workflow run."""

    lookback_days: int | None = None
    thread_lookback_days: int | None = None
    limit: int = DEFAULT_CHANNEL_PAGE_LIMIT
    thread_reply_limit: int = DEFAULT_THREAD_REPLY_PAGE_LIMIT
    oldest: str | None = None
    latest: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)


def _ts_minus_days(ts: str | None, days: int) -> str | None:
    """Move a Slack timestamp back by a whole-day lookback window."""
    if not ts:
        return None
    try:
        seconds = max(float(ts) - (days * 86_400), 0.0)
    except (TypeError, ValueError):
        return None
    return f"{seconds:.6f}"


def _ts_now_minus_days(days: int) -> str:
    """Return the current time minus a whole-day window as a Slack timestamp."""
    now = dt.datetime.now(dt.timezone.utc).timestamp()
    return f"{max(now - (days * 86_400), 0.0):.6f}"


def _bootstrap_backfill_job_key(channel_id: str) -> str:
    """Return the stable job key for a channel's initial historical bootstrap."""
    return f"bootstrap:{channel_id}"


def _continuation_backfill_job_key(
    channel_id: str,
    *,
    oldest_ts: str | None,
    latest_ts: str | None,
) -> str:
    """Return the stable job key for draining one bounded history window."""
    return f"continuation:{channel_id}:{oldest_ts or ''}:{latest_ts or ''}"


def _thread_refresh_job_key(channel_id: str, thread_ts: str) -> str:
    """Return the stable job key for refreshing one thread's full reply set."""
    return f"thread_refresh:{channel_id}:{thread_ts}"


def _ts_within_days(ts: str | None, days: int, *, now: dt.datetime) -> bool:
    """Return whether a Slack ts falls within the recent thread refresh window."""
    if not ts:
        return False
    try:
        occurred_at = dt.datetime.fromtimestamp(float(ts), tz=dt.timezone.utc)
    except (TypeError, ValueError, OSError):
        return False
    return occurred_at >= now - dt.timedelta(days=days)


async def _upsert_channels(pool, channels: list[dict[str, Any]]) -> None:
    """Refresh public Slack sync channel rows and mark absent channels out of scope."""
    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute(
                "UPDATE slack_sync_channels SET is_syncable = FALSE, updated_at = NOW()",
            )
            for channel in channels:
                channel_id = str(channel.get("id") or "")
                if not channel_id:
                    continue
                await conn.execute(
                    "INSERT INTO slack_sync_channels ("
                    "channel_id, channel_name, is_archived, is_syncable, topic, purpose, "
                    "member_count, raw_payload, last_seen_at, updated_at"
                    ") VALUES ($1, $2, $3, TRUE, $4, $5, $6, $7::jsonb, NOW(), NOW()) "
                    "ON CONFLICT (channel_id) DO UPDATE SET "
                    "channel_name = EXCLUDED.channel_name, "
                    "is_archived = EXCLUDED.is_archived, "
                    "is_syncable = TRUE, "
                    "topic = EXCLUDED.topic, "
                    "purpose = EXCLUDED.purpose, "
                    "member_count = EXCLUDED.member_count, "
                    "raw_payload = EXCLUDED.raw_payload, "
                    "last_seen_at = NOW(), "
                    "updated_at = NOW()",
                    channel_id,
                    str(channel.get("name") or ""),
                    bool(channel.get("is_archived")),
                    str(channel.get("topic") or ""),
                    str(channel.get("purpose") or ""),
                    int(channel.get("member_count") or 0),
                    canonical_json(channel),
                )


async def _upsert_users(pool, users: list[dict[str, Any]]) -> int:
    """Refresh Slack user directory rows."""
    async with pool.acquire() as conn:
        async with conn.transaction():
            for user in users:
                user_id = str(user.get("id") or "")
                if not user_id:
                    continue
                profile = (
                    user.get("profile") if isinstance(user.get("profile"), dict) else {}
                )
                await conn.execute(
                    "INSERT INTO slack_sync_users ("
                    "user_id, user_name, real_name, display_name, is_bot, is_deleted, "
                    "team_id, raw_payload, last_seen_at, updated_at"
                    ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, NOW(), NOW()) "
                    "ON CONFLICT (user_id) DO UPDATE SET "
                    "user_name = EXCLUDED.user_name, "
                    "real_name = EXCLUDED.real_name, "
                    "display_name = EXCLUDED.display_name, "
                    "is_bot = EXCLUDED.is_bot, "
                    "is_deleted = EXCLUDED.is_deleted, "
                    "team_id = EXCLUDED.team_id, "
                    "raw_payload = EXCLUDED.raw_payload, "
                    "last_seen_at = NOW(), "
                    "updated_at = NOW()",
                    user_id,
                    str(user.get("name") or ""),
                    str(user.get("real_name") or ""),
                    str(user.get("display_name") or profile.get("display_name") or ""),
                    bool(user.get("is_bot")),
                    bool(user.get("deleted") or user.get("is_deleted")),
                    str(user.get("team_id") or user.get("team") or ""),
                    canonical_json(user),
                )
    return len([u for u in users if u.get("id")])


async def _load_checkpoint(pool, channel_id: str) -> dict[str, Any] | None:
    """Load the current per-channel sync checkpoint."""
    row = await pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints WHERE channel_id = $1",
        channel_id,
    )
    return dict(row) if row else None


def _client():
    """Compatibility wrapper for tests patching the old helper."""
    return shared_client(workflow_name=WORKFLOW_NAME)


async def _upsert_messages(pool, rows: list[dict[str, Any]]) -> int:
    """Compatibility wrapper for tests patching the old helper."""
    return await upsert_messages(pool, rows)


async def _update_checkpoint_success(
    pool,
    *,
    channel_id: str,
    watermark_ts: str | None,
    run_id: str,
) -> None:
    """Advance a channel checkpoint after all writes for that channel succeed."""
    await pool.execute(
        "INSERT INTO slack_sync_checkpoints ("
        "channel_id, watermark_ts, last_run_id, last_success_at, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW(), '', NOW()) "
        "ON CONFLICT (channel_id) DO UPDATE SET "
        "watermark_ts = EXCLUDED.watermark_ts, "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_success_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        channel_id,
        watermark_ts,
        run_id,
    )


async def _update_checkpoint_failure(
    pool,
    *,
    channel_id: str,
    run_id: str,
    error: str,
) -> None:
    """Record channel failure details without advancing the watermark."""
    await pool.execute(
        "INSERT INTO slack_sync_checkpoints ("
        "channel_id, last_run_id, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW()) "
        "ON CONFLICT (channel_id) DO UPDATE SET "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_error = EXCLUDED.last_error, "
        "updated_at = NOW()",
        channel_id,
        run_id,
        error,
    )


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Sync public Slack channels visible through the configured ETL user token."""
    if not _env_flag_enabled("SLACK_ETL_ENABLED"):
        ctx.log("slack_sync_skipped_disabled")
        return {
            "status": "skipped",
            "reason": "slack_etl_disabled",
            "channels_skipped": [],
        }

    lookback_days = positive_int(
        inp.lookback_days or os.getenv("SLACK_SYNC_BACKFILL_LOOKBACK_DAYS"),
        DEFAULT_LOOKBACK_DAYS,
    )
    thread_lookback_days = positive_int(
        inp.thread_lookback_days or os.getenv("SLACK_SYNC_THREAD_LOOKBACK_DAYS"),
        DEFAULT_THREAD_LOOKBACK_DAYS,
    )
    limit = positive_int(inp.limit, DEFAULT_CHANNEL_PAGE_LIMIT)
    client = _client()
    access_mode = client._etl_access_mode()
    public_channels = client._list_etl_channels(limit=10_000, force_refresh=True)
    record_etl_items_seen("slack", "channel", "channel", len(public_channels))
    exclusion_patterns = _channel_exclusion_patterns(os.getenv(EXCLUDED_CHANNELS_ENV))
    channels_to_sync, excluded_channels = _filter_excluded_channels(
        public_channels,
        exclusion_patterns,
    )
    if excluded_channels:
        ctx.log(
            "slack_sync_channels_excluded",
            count=len(excluded_channels),
            patterns=exclusion_patterns,
            channels=excluded_channels,
        )
    await _upsert_channels(ctx._pool, channels_to_sync)
    record_etl_items_upserted("slack", "channel", "channel", len(channels_to_sync))

    if not public_channels:
        reason = "no_public_channels"
        ctx.log(
            "slack_sync_skipped_no_public_channels",
            access_mode=access_mode,
            reason=reason,
        )
        await emit_slack_checkpoint_metrics(ctx._pool)
        return {
            "status": "skipped",
            "reason": reason,
            "channels_skipped": [],
        }

    if not channels_to_sync:
        reason = "all_channels_excluded"
        ctx.log(
            "slack_sync_skipped_all_channels_excluded",
            access_mode=access_mode,
            reason=reason,
            channels_skipped=excluded_channels,
        )
        await emit_slack_checkpoint_metrics(ctx._pool)
        return {
            "status": "skipped",
            "reason": reason,
            "channels_skipped": excluded_channels,
        }

    users = client._list_etl_users(limit=10_000)
    record_etl_items_seen("slack", "user", "user", len(users))
    users_upserted = await _upsert_users(ctx._pool, users)
    record_etl_items_upserted("slack", "user", "user", users_upserted)

    run_id = workflow_run_id_to_sync_run_id(ctx.run_id)
    await record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        mode="incremental",
        requested=[channel_ref(channel) for channel in channels_to_sync],
        skipped=excluded_channels,
        metadata={
            **inp.metadata,
            "slack_access_mode": access_mode,
            "users_upserted": users_upserted,
            "excluded_channel_patterns": exclusion_patterns,
        },
    )

    synced: list[dict[str, str]] = []
    skipped: list[dict[str, str]] = list(excluded_channels)
    failed: list[dict[str, str]] = []
    counts = {
        "messages_fetched": 0,
        "messages_upserted": 0,
        "threads_fetched": 0,
        "replies_fetched": 0,
        "replies_upserted": 0,
    }

    for channel in channels_to_sync:
        channel_id = str(channel.get("id") or "")
        channel_name = str(channel.get("name") or channel_id)
        try:
            checkpoint = await _load_checkpoint(ctx._pool, channel_id)
            checkpoint_watermark = (
                checkpoint.get("watermark_ts") if checkpoint else None
            )
            state = {
                "cursor": None,
                "watermark": checkpoint_watermark,
                "oldest": None,
                "latest": None,
            }
            oldest = inp.oldest
            if oldest is None:
                if state.get("watermark"):
                    oldest = _ts_minus_days(
                        str(state["watermark"]), thread_lookback_days
                    )
                else:
                    oldest = _ts_now_minus_days(lookback_days)

            page = client._sync_etl_channel_history(
                channel_id,
                state=state,
                limit=limit,
                lookback_days=lookback_days,
                oldest=oldest,
                latest=inp.latest,
            )
            messages = page.get("messages") or []
            message_rows = [message_row(msg, run_id) for msg in messages]
            counts["messages_fetched"] += len(message_rows)
            record_etl_items_seen("slack", "channel", "root_message", len(message_rows))
            messages_upserted = await _upsert_messages(ctx._pool, message_rows)
            counts["messages_upserted"] += messages_upserted
            record_etl_items_upserted(
                "slack",
                "channel",
                "root_message",
                messages_upserted,
            )

            thread_roots = {
                str(msg.get("timestamp"))
                for msg in messages
                if msg.get("timestamp") and int(msg.get("reply_count") or 0) > 0
            }
            refresh_now = dt.datetime.now(dt.timezone.utc)
            refresh_cutoff = refresh_now - dt.timedelta(
                hours=DEFAULT_THREAD_REFRESH_INTERVAL_HOURS
            )
            thread_refresh_times = await load_thread_refresh_times(
                ctx._pool,
                channel_id=channel_id,
                thread_ts_values=sorted(thread_roots),
            )
            for thread_ts in sorted(thread_roots):
                if not _ts_within_days(
                    thread_ts, thread_lookback_days, now=refresh_now
                ):
                    continue
                last_refreshed_at = thread_refresh_times.get(thread_ts)
                if (
                    last_refreshed_at is not None
                    and last_refreshed_at >= refresh_cutoff
                ):
                    continue
                await enqueue_backfill_job(
                    ctx._pool,
                    job_key=_thread_refresh_job_key(channel_id, thread_ts),
                    job_type=BACKFILL_JOB_THREAD_REFRESH,
                    channel_id=channel_id,
                    payload={"thread_ts": thread_ts},
                    run_id=run_id,
                    priority=200,
                )
                record_etl_items_enqueued("slack", "channel", "thread_refresh_job", 1)
                ctx.log(
                    "slack_sync_backfill_enqueued",
                    channel_id=channel_id,
                    channel_name=channel_name,
                    job_type=BACKFILL_JOB_THREAD_REFRESH,
                    job_key=_thread_refresh_job_key(channel_id, thread_ts),
                    thread_ts=thread_ts,
                    refresh_interval_hours=DEFAULT_THREAD_REFRESH_INTERVAL_HOURS,
                )

            next_state = page.get("sync_state") or {}
            initial_backfill_seeded = False
            bootstrap_widened = False
            if checkpoint_watermark is not None and inp.oldest is None:
                desired_oldest = _ts_now_minus_days(lookback_days)
                bootstrap_widened = await widen_channel_bootstrap_job(
                    ctx._pool,
                    channel_id=channel_id,
                    window_oldest=desired_oldest,
                    lookback_days=lookback_days,
                    thread_lookback_days=thread_lookback_days,
                    run_id=run_id,
                    priority=150,
                )
                if bootstrap_widened:
                    record_etl_items_enqueued(
                        "slack", "channel", "channel_bootstrap_widened_job", 1
                    )
                    ctx.log(
                        "slack_sync_bootstrap_widened",
                        channel_id=channel_id,
                        channel_name=channel_name,
                        job_type=BACKFILL_JOB_CHANNEL_BOOTSTRAP,
                        job_key=_bootstrap_backfill_job_key(channel_id),
                        lookback_days=lookback_days,
                        window_oldest_ts=desired_oldest,
                    )
            if next_state.get("cursor"):
                await enqueue_backfill_job(
                    ctx._pool,
                    job_key=_continuation_backfill_job_key(
                        channel_id,
                        oldest_ts=str(next_state.get("oldest") or "") or None,
                        latest_ts=str(next_state.get("latest") or "") or None,
                    ),
                    job_type=BACKFILL_JOB_CHANNEL_CONTINUATION,
                    channel_id=channel_id,
                    payload={
                        "cursor": next_state.get("cursor"),
                        "oldest": next_state.get("oldest"),
                        "latest": next_state.get("latest"),
                        "lookback_days": lookback_days,
                        "thread_lookback_days": thread_lookback_days,
                    },
                    run_id=run_id,
                    priority=100,
                )
                continuation_job_key = _continuation_backfill_job_key(
                    channel_id,
                    oldest_ts=str(next_state.get("oldest") or "") or None,
                    latest_ts=str(next_state.get("latest") or "") or None,
                )
                record_etl_items_enqueued(
                    "slack", "channel", "channel_continuation_job", 1
                )
                ctx.log(
                    "slack_sync_backfill_enqueued",
                    channel_id=channel_id,
                    channel_name=channel_name,
                    job_type=BACKFILL_JOB_CHANNEL_CONTINUATION,
                    job_key=continuation_job_key,
                    oldest_ts=next_state.get("oldest"),
                    latest_ts=next_state.get("latest"),
                    has_cursor=True,
                )
            await _update_checkpoint_success(
                ctx._pool,
                channel_id=channel_id,
                watermark_ts=next_state.get("watermark"),
                run_id=run_id,
            )
            synced.append(channel_ref(channel))
            ctx.log(
                "slack_sync_channel_completed",
                channel_id=channel_id,
                channel_name=channel_name,
                messages=len(message_rows),
                threads=len(thread_roots),
                backfill_seeded=initial_backfill_seeded,
                bootstrap_widened=bootstrap_widened,
                backfill_continuation=bool(next_state.get("cursor")),
            )
        except Exception as exc:
            error = str(exc)
            ctx.log(
                "slack_sync_channel_failed",
                channel_id=channel_id,
                channel_name=channel_name,
                error=error,
            )
            failed.append(channel_ref(channel, error))
            record_etl_items_failed(
                "slack",
                "channel",
                "channel",
                failure_reason(error),
            )
            await _update_checkpoint_failure(
                ctx._pool,
                channel_id=channel_id,
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
    await emit_slack_checkpoint_metrics(ctx._pool)

    return {
        "status": status,
        "run_id": run_id,
        "channels_synced": len(synced),
        "channels_skipped": len(skipped),
        "channels_failed": len(failed),
        **counts,
    }
