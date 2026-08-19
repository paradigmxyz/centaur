"""Workflow: prune old Slack ETL and Slack DM rows from Postgres."""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Any

from workflows.etl_metrics import record_etl_items_deleted
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import env_flag_enabled, positive_int

WORKFLOW_NAME = "slack_retention"

DEFAULT_RETENTION_INTERVAL_MINUTES = 60
DISABLED_RETENTION_DAYS = 0


def _nonnegative_int(value: int | str | None, default: int) -> int:
    """Coerce nonnegative integer config values with a safe default."""
    try:
        parsed = int(value) if value is not None else default
    except (TypeError, ValueError):
        return default
    return parsed if parsed >= 0 else default


def _configured_etl_retention_days() -> int:
    return _nonnegative_int(
        os.getenv("SLACK_ETL_RETENTION_DAYS"),
        DISABLED_RETENTION_DAYS,
    )


def _configured_dm_retention_days() -> int:
    return _nonnegative_int(
        os.getenv("SLACK_DM_RETENTION_DAYS"),
        DISABLED_RETENTION_DAYS,
    )


def _schedule_enabled() -> bool:
    if not env_flag_enabled("SLACK_RETENTION_ENABLED", default=True):
        return False
    return _configured_etl_retention_days() > 0 or _configured_dm_retention_days() > 0


SCHEDULE = {
    "schedule_id": "slack_retention",
    "interval_seconds": positive_int(
        os.getenv("SLACK_RETENTION_INTERVAL_MINUTES"),
        DEFAULT_RETENTION_INTERVAL_MINUTES,
    )
    * 60,
    "enabled": _schedule_enabled(),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for Slack retention cleanup."""

    etl_retention_days: int | None = None
    dm_retention_days: int | None = None
    dry_run: bool = False
    metadata: dict[str, Any] = field(default_factory=dict)


async def _count_or_delete(
    pool,
    *,
    count_sql: str,
    delete_sql: str,
    days: int,
    dry_run: bool,
) -> int:
    sql = count_sql if dry_run else delete_sql
    deleted = await pool.fetchval(sql, days)
    return int(deleted or 0)


async def prune_slack_etl(pool, *, retention_days: int, dry_run: bool = False) -> dict[str, int]:
    """Delete Slack ETL rows older than the configured retention window."""
    if retention_days <= 0:
        return {
            "company_context_documents": 0,
            "messages": 0,
            "backfill_jobs": 0,
            "runs": 0,
        }

    counts = {
        "company_context_documents": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM company_context_documents "
                "WHERE source = 'slack' "
                "  AND occurred_at < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM company_context_documents "
                "    WHERE source = 'slack' "
                "      AND occurred_at < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
        "messages": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_sync_messages "
                "WHERE occurred_at < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_sync_messages "
                "    WHERE occurred_at < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
        "backfill_jobs": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_sync_backfill_jobs "
                "WHERE status IN ('completed', 'failed') "
                "  AND updated_at < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_sync_backfill_jobs "
                "    WHERE status IN ('completed', 'failed') "
                "      AND updated_at < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
        "runs": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_sync_runs "
                "WHERE status <> 'running' "
                "  AND COALESCE(finished_at, started_at) < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_sync_runs "
                "    WHERE status <> 'running' "
                "      AND COALESCE(finished_at, started_at) "
                "          < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
    }
    return counts


async def prune_slack_dm(pool, *, retention_days: int, dry_run: bool = False) -> dict[str, int]:
    """Delete Slack DM sync rows older than the configured retention window."""
    if retention_days <= 0:
        return {
            "messages": 0,
            "conversations": 0,
            "backfill_jobs": 0,
            "runs": 0,
        }

    counts = {
        "messages": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_private_sync_messages "
                "WHERE occurred_at < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_private_sync_messages "
                "    WHERE occurred_at < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
        "conversations": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_private_sync_conversations c "
                "WHERE c.last_seen_at < NOW() - make_interval(days => $1) "
                "  AND NOT EXISTS ("
                "      SELECT 1 FROM slack_private_sync_messages m "
                "      WHERE m.home_team_id = c.home_team_id "
                "        AND m.conversation_id = c.conversation_id"
                "  )"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_private_sync_conversations c "
                "    WHERE c.last_seen_at < NOW() - make_interval(days => $1) "
                "      AND NOT EXISTS ("
                "          SELECT 1 FROM slack_private_sync_messages m "
                "          WHERE m.home_team_id = c.home_team_id "
                "            AND m.conversation_id = c.conversation_id"
                "      ) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
        "backfill_jobs": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_private_sync_backfill_jobs "
                "WHERE status IN ('completed', 'failed') "
                "  AND updated_at < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_private_sync_backfill_jobs "
                "    WHERE status IN ('completed', 'failed') "
                "      AND updated_at < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
        "runs": await _count_or_delete(
            pool,
            count_sql=(
                "SELECT COUNT(*) FROM slack_private_sync_runs "
                "WHERE status <> 'running' "
                "  AND COALESCE(finished_at, started_at) < NOW() - make_interval(days => $1)"
            ),
            delete_sql=(
                "WITH deleted AS ("
                "    DELETE FROM slack_private_sync_runs "
                "    WHERE status <> 'running' "
                "      AND COALESCE(finished_at, started_at) "
                "          < NOW() - make_interval(days => $1) "
                "    RETURNING 1"
                ") SELECT COUNT(*) FROM deleted"
            ),
            days=retention_days,
            dry_run=dry_run,
        ),
    }
    return counts


def _emit_deleted_metrics(prefix: str, counts: dict[str, int]) -> None:
    for item_type, count in counts.items():
        if count:
            record_etl_items_deleted(prefix, "retention", item_type, count)


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    etl_days = (
        _nonnegative_int(inp.etl_retention_days, DISABLED_RETENTION_DAYS)
        if inp.etl_retention_days is not None
        else _configured_etl_retention_days()
    )
    dm_days = (
        _nonnegative_int(inp.dm_retention_days, DISABLED_RETENTION_DAYS)
        if inp.dm_retention_days is not None
        else _configured_dm_retention_days()
    )

    etl_counts = await prune_slack_etl(
        ctx._pool,
        retention_days=etl_days,
        dry_run=inp.dry_run,
    )
    dm_counts = await prune_slack_dm(
        ctx._pool,
        retention_days=dm_days,
        dry_run=inp.dry_run,
    )

    if not inp.dry_run:
        _emit_deleted_metrics("slack", etl_counts)
        _emit_deleted_metrics("slack_dm", dm_counts)

    return {
        "ok": True,
        "dry_run": inp.dry_run,
        "etl_retention_days": etl_days,
        "dm_retention_days": dm_days,
        "slack_etl": etl_counts,
        "slack_dm": dm_counts,
        "metadata": inp.metadata,
    }
