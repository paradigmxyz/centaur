"""Shared helpers for Slack ETL incremental sync and backfill workflows."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import mimetypes
import os
import re
import time
from collections.abc import Callable
from typing import Any, ClassVar, Protocol
from urllib import error as urllib_error
from urllib import request as urllib_request

from centaur_sdk import secret
from api.runtime_control import canonical_json

FALSE_ENV_VALUES = {"0", "false", "no", "off"}
DEFAULT_ATTACHMENT_MAX_BYTES = 10 * 1024 * 1024
BACKFILL_JOB_CHANNEL_CONTINUATION = "channel_continuation"
BACKFILL_JOB_CHANNEL_BOOTSTRAP = "channel_bootstrap"
BACKFILL_JOB_THREAD_REFRESH = "thread_refresh"
BACKFILL_JOB_PAYLOAD_VERSION = 1
BACKFILL_JOB_TYPES = (
    BACKFILL_JOB_CHANNEL_BOOTSTRAP,
    BACKFILL_JOB_CHANNEL_CONTINUATION,
    BACKFILL_JOB_THREAD_REFRESH,
)
BACKFILL_JOB_STATUSES = ("pending", "running", "completed", "failed")
PERMANENT_SLACK_BACKFILL_ERRORS = (
    "channel_not_found",
    "thread_not_found",
)


def positive_int(value: int | str | None, default: int) -> int:
    """Coerce positive integer config values with a safe default."""
    try:
        parsed = int(value) if value is not None else default
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default


def env_flag_enabled(name: str, default: bool = True) -> bool:
    """Read a boolean feature flag where common false strings opt out."""
    value = os.getenv(name)
    if value is None:
        return default
    return value.strip().lower() not in FALSE_ENV_VALUES


def attachment_download_enabled() -> bool:
    """Return whether Slack ETL should download attachment bytes into Postgres."""
    return env_flag_enabled("SLACK_ETL_ATTACHMENTS_ENABLED", default=True)


def attachment_max_bytes() -> int:
    """Return the per-file Slack ETL attachment byte cap."""
    return positive_int(
        os.getenv("SLACK_ETL_ATTACHMENT_MAX_BYTES"),
        DEFAULT_ATTACHMENT_MAX_BYTES,
    )


class SlackSyncClient(Protocol):
    """Small protocol for the Slack client methods used by Slack ETL workflows."""

    def _etl_access_mode(self) -> str: ...

    def _list_etl_channels(
        self, limit: int = 200, force_refresh: bool = False
    ) -> list[dict]: ...

    def _list_etl_users(self, limit: int = 200) -> list[dict]: ...

    def _sync_etl_channel_history(
        self,
        channel: str,
        state: dict[str, Any] | None = None,
        limit: int = 200,
        lookback_days: int = 30,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
    ) -> dict[str, Any]: ...

    def _get_etl_thread_replies_page(
        self,
        channel: str,
        thread_ts: str,
        limit: int = 200,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = True,
    ) -> dict[str, Any]: ...


def slack_ts_to_datetime(ts: str | None) -> dt.datetime | None:
    """Convert Slack timestamp strings to UTC datetimes for indexed queries."""
    if not ts:
        return None
    try:
        return dt.datetime.fromtimestamp(float(ts), tz=dt.timezone.utc)
    except (TypeError, ValueError, OSError):
        return None


def message_thread_ts(message: dict[str, Any]) -> str | None:
    """Return the thread root timestamp for a normalized Slack message."""
    thread_ts = message.get("thread_ts")
    if isinstance(thread_ts, str) and thread_ts.strip():
        return thread_ts.strip()
    message_ts = message.get("timestamp")
    reply_count = message.get("reply_count")
    if isinstance(message_ts, str) and isinstance(reply_count, int) and reply_count > 0:
        return message_ts
    return None


def message_row(
    message: dict[str, Any],
    run_id: str,
    parent_message_ts: str | None = None,
) -> dict[str, Any]:
    """Project a normalized Slack message into the DB upsert shape."""
    message_ts = str(message.get("timestamp") or "")
    thread_ts = message_thread_ts(message)
    user_id = str(message.get("user_id") or "")
    bot_id = str(message.get("bot_id") or "")
    return {
        "channel_id": str(message.get("channel_id") or ""),
        "message_ts": message_ts,
        "occurred_at": slack_ts_to_datetime(message_ts),
        "thread_ts": thread_ts,
        "parent_message_ts": parent_message_ts,
        "is_thread_root": bool(thread_ts and thread_ts == message_ts),
        "user_id": user_id,
        "bot_id": bot_id,
        "message_type": str(message.get("type") or "message"),
        "message_subtype": message.get("subtype"),
        "text": str(message.get("text") or ""),
        "permalink": str(message.get("permalink") or ""),
        "reply_count": int(message.get("reply_count") or 0),
        "reply_users": message.get("reply_users") or [],
        "latest_reply_ts": message.get("latest_reply"),
        "raw_payload": _message_raw_payload(message),
        "attachments": message.get("files") or [],
        "source_run_id": run_id,
    }


def _message_raw_payload(message: dict[str, Any]) -> dict[str, Any]:
    """Return the message payload without byte content so it can be stored as JSONB."""
    raw_payload = dict(message)
    files = raw_payload.get("files")
    if isinstance(files, list):
        raw_payload["files"] = [
            _attachment_raw_payload(file_obj)
            for file_obj in files
            if isinstance(file_obj, dict)
        ]
    return raw_payload


def _attachment_raw_payload(attachment: dict[str, Any]) -> dict[str, Any]:
    raw_payload = attachment.get("raw_payload")
    if isinstance(raw_payload, dict):
        return raw_payload
    return {
        key: value for key, value in attachment.items() if key not in {"content_bytes"}
    }


def _safe_int(value: Any, default: int = 0) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    return parsed if parsed >= 0 else default


def _attachment_rows(row: dict[str, Any]) -> list[dict[str, Any]]:
    """Project normalized Slack file metadata into DB rows."""
    attachments = row.get("attachments") or []
    if not isinstance(attachments, list):
        return []

    result: list[dict[str, Any]] = []
    for attachment in attachments:
        if not isinstance(attachment, dict):
            continue
        slack_file_id = str(attachment.get("id") or "").strip()
        if not slack_file_id:
            continue
        result.append(
            {
                "channel_id": row["channel_id"],
                "message_ts": row["message_ts"],
                "slack_file_id": slack_file_id,
                "name": str(attachment.get("name") or ""),
                "title": str(attachment.get("title") or ""),
                "mimetype": str(attachment.get("mimetype") or ""),
                "filetype": str(attachment.get("filetype") or ""),
                "size_bytes": _safe_int(attachment.get("size")),
                "url_private": str(attachment.get("url_private") or ""),
                "permalink": str(attachment.get("permalink") or ""),
                "download_status": str(
                    attachment.get("download_status") or "metadata_only"
                ),
                "download_error": str(attachment.get("download_error") or ""),
                "content_sha256": attachment.get("content_sha256"),
                "content_bytes": attachment.get("content_bytes"),
                "raw_payload": _attachment_raw_payload(attachment),
                "source_run_id": row["source_run_id"],
            }
        )
    return result


_ATTACHMENT_UPSERT_SQL = (
    "INSERT INTO slack_sync_message_attachments ("
    "channel_id, message_ts, slack_file_id, name, title, mimetype, filetype, "
    "size_bytes, url_private, permalink, download_status, download_error, "
    "content_sha256, content_bytes, raw_payload, source_run_id, last_seen_at, updated_at"
    ") VALUES ("
    "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, "
    "$15::jsonb, $16, NOW(), NOW()"
    ") ON CONFLICT (channel_id, message_ts, slack_file_id) DO UPDATE SET "
    "name = EXCLUDED.name, "
    "title = EXCLUDED.title, "
    "mimetype = EXCLUDED.mimetype, "
    "filetype = EXCLUDED.filetype, "
    "size_bytes = EXCLUDED.size_bytes, "
    "url_private = EXCLUDED.url_private, "
    "permalink = EXCLUDED.permalink, "
    "download_status = EXCLUDED.download_status, "
    "download_error = EXCLUDED.download_error, "
    "content_sha256 = EXCLUDED.content_sha256, "
    "content_bytes = EXCLUDED.content_bytes, "
    "raw_payload = EXCLUDED.raw_payload, "
    "source_run_id = EXCLUDED.source_run_id, "
    "last_seen_at = NOW(), "
    "updated_at = NOW()"
)


def _attachment_upsert_params(attachment: dict[str, Any]) -> tuple[Any, ...]:
    """Positional parameters for ``_ATTACHMENT_UPSERT_SQL`` for one attachment."""
    return (
        attachment["channel_id"],
        attachment["message_ts"],
        attachment["slack_file_id"],
        attachment["name"],
        attachment["title"],
        attachment["mimetype"],
        attachment["filetype"],
        attachment["size_bytes"],
        attachment["url_private"],
        attachment["permalink"],
        attachment["download_status"],
        attachment["download_error"],
        attachment["content_sha256"],
        attachment["content_bytes"],
        canonical_json(attachment["raw_payload"]),
        attachment["source_run_id"],
    )


async def _replace_message_attachments_batch(conn, rows: list[dict[str, Any]]) -> None:
    """Replace attachment rows for a batch of observed Slack messages.

    Upserts every attachment across the batch with a single ``executemany`` and
    drops attachments that are no longer present with one set-based delete,
    rather than a statement per message. Semantics match the previous per-row
    reconciliation: for each message in the batch, attachments not in the freshly
    observed set are removed (messages with no attachments have all of theirs
    removed).
    """
    if not rows:
        return

    attachment_params: list[tuple[Any, ...]] = []
    kept_channel_ids: list[str] = []
    kept_message_ts: list[str] = []
    kept_file_ids: list[str] = []
    for row in rows:
        for attachment in _attachment_rows(row):
            attachment_params.append(_attachment_upsert_params(attachment))
            kept_channel_ids.append(attachment["channel_id"])
            kept_message_ts.append(attachment["message_ts"])
            kept_file_ids.append(attachment["slack_file_id"])

    if attachment_params:
        await conn.executemany(_ATTACHMENT_UPSERT_SQL, attachment_params)

    message_channel_ids = [row["channel_id"] for row in rows]
    message_ts = [row["message_ts"] for row in rows]
    await conn.execute(
        "DELETE FROM slack_sync_message_attachments a "
        "USING unnest($1::text[], $2::text[]) AS m(channel_id, message_ts) "
        "WHERE a.channel_id = m.channel_id "
        "  AND a.message_ts = m.message_ts "
        "  AND NOT EXISTS ("
        "    SELECT 1 FROM unnest($3::text[], $4::text[], $5::text[]) "
        "      AS k(channel_id, message_ts, slack_file_id) "
        "    WHERE k.channel_id = a.channel_id "
        "      AND k.message_ts = a.message_ts "
        "      AND k.slack_file_id = a.slack_file_id"
        "  )",
        message_channel_ids,
        message_ts,
        kept_channel_ids,
        kept_message_ts,
        kept_file_ids,
    )


def channel_ref(channel: dict[str, Any], reason: str | None = None) -> dict[str, str]:
    """Return a compact channel reference for run summaries."""
    result = {
        "channel_id": str(channel.get("id") or ""),
        "channel_name": str(channel.get("name") or ""),
    }
    if reason:
        result["reason"] = reason
    return result


def failure_reason(error: str) -> str:
    """Map Slack/client errors to low-cardinality metric reasons."""
    lowered = error.lower()
    if "rate_limited" in lowered or "ratelimited" in lowered:
        return "rate_limited"
    if (
        "missing_scope" in lowered
        or "not_in_channel" in lowered
        or "permission" in lowered
    ):
        return "permission_error"
    if "repeated reply cursor" in lowered or "cursor" in lowered:
        return "cursor_error"
    if "slack api" in lowered or "slack_sdk" in lowered:
        return "api_error"
    if "write" in lowered or "database" in lowered or "postgres" in lowered:
        return "write_error"
    return "unknown_error"


def is_permanent_slack_backfill_error(error: str) -> bool:
    """Return whether a Slack backfill error should terminally skip the job."""
    lowered = error.lower()
    return any(
        f"slack api error: {error_code}" in lowered
        for error_code in PERMANENT_SLACK_BACKFILL_ERRORS
    )


_MESSAGE_UPSERT_SQL = (
    "INSERT INTO slack_sync_messages ("
    "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, "
    "is_thread_root, user_id, bot_id, message_type, message_subtype, text, "
    "permalink, reply_count, reply_users, latest_reply_ts, raw_payload, "
    "source_run_id, last_seen_at, updated_at"
    ") VALUES ("
    "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, "
    "$14::jsonb, $15, $16::jsonb, $17, NOW(), NOW()"
    ") ON CONFLICT (channel_id, message_ts) DO UPDATE SET "
    "occurred_at = EXCLUDED.occurred_at, "
    "thread_ts = EXCLUDED.thread_ts, "
    "parent_message_ts = EXCLUDED.parent_message_ts, "
    "is_thread_root = EXCLUDED.is_thread_root, "
    "user_id = EXCLUDED.user_id, "
    "bot_id = EXCLUDED.bot_id, "
    "message_type = EXCLUDED.message_type, "
    "message_subtype = EXCLUDED.message_subtype, "
    "text = EXCLUDED.text, "
    "permalink = EXCLUDED.permalink, "
    "reply_count = EXCLUDED.reply_count, "
    "reply_users = EXCLUDED.reply_users, "
    "latest_reply_ts = EXCLUDED.latest_reply_ts, "
    "raw_payload = EXCLUDED.raw_payload, "
    "source_run_id = EXCLUDED.source_run_id, "
    "last_seen_at = NOW(), "
    "updated_at = NOW()"
)


def _message_upsert_params(row: dict[str, Any]) -> tuple[Any, ...]:
    """Positional parameters for ``_MESSAGE_UPSERT_SQL`` for one message row."""
    return (
        row["channel_id"],
        row["message_ts"],
        row["occurred_at"],
        row["thread_ts"],
        row["parent_message_ts"],
        row["is_thread_root"],
        row["user_id"],
        row["bot_id"],
        row["message_type"],
        row["message_subtype"],
        row["text"],
        row["permalink"],
        row["reply_count"],
        canonical_json(row["reply_users"]),
        row["latest_reply_ts"],
        canonical_json(row["raw_payload"]),
        row["source_run_id"],
    )


def _dedupe_message_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Collapse duplicate channel/message rows so attachment reconciliation is last-write-wins."""
    by_key: dict[tuple[str, str], dict[str, Any]] = {}
    for row in rows:
        by_key[(row["channel_id"], row["message_ts"])] = row
    return list(by_key.values())


async def upsert_messages(pool, rows: list[dict[str, Any]]) -> int:
    """Upsert Slack messages and replies by their channel-scoped Slack ts.

    The whole batch is written with set-based statements (one ``executemany``
    for the messages, one for their attachments, and a single stale-attachment
    delete) instead of one statement per row. Backfill jobs upsert thousands of
    messages at a time; the previous per-row loop kept one long transaction open
    and saturated Postgres, starving the interactive session path that runs on
    the same database.
    """
    if not rows:
        return 0
    deduped_rows = _dedupe_message_rows(rows)
    message_params = [_message_upsert_params(row) for row in deduped_rows]
    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.executemany(_MESSAGE_UPSERT_SQL, message_params)
            await _replace_message_attachments_batch(conn, deduped_rows)
    return len(rows)


async def load_thread_refresh_times(
    pool,
    *,
    channel_id: str,
    thread_ts_values: list[str],
) -> dict[str, dt.datetime | None]:
    """Load last refresh timestamps for root messages keyed by thread ts."""
    if not thread_ts_values:
        return {}
    rows = await pool.fetch(
        "SELECT message_ts, thread_refreshed_at "
        "FROM slack_sync_messages "
        "WHERE channel_id = $1 "
        "  AND is_thread_root = TRUE "
        "  AND message_ts = ANY($2::text[])",
        channel_id,
        thread_ts_values,
    )
    return {
        str(row["message_ts"]): (
            row["thread_refreshed_at"].astimezone(dt.timezone.utc)
            if isinstance(row["thread_refreshed_at"], dt.datetime)
            else None
        )
        for row in rows
    }


async def replace_thread_replies(
    pool,
    *,
    channel_id: str,
    thread_ts: str,
    reply_rows: list[dict[str, Any]],
) -> tuple[int, int]:
    """Replace the stored reply set for one thread with the fetched authoritative set."""
    upserted = await upsert_messages(pool, reply_rows)
    reply_ts_values = [
        str(row["message_ts"]) for row in reply_rows if row.get("message_ts")
    ]
    async with pool.acquire() as conn:
        async with conn.transaction():
            if reply_ts_values:
                deleted = await conn.fetchval(
                    "WITH deleted AS ("
                    "    DELETE FROM slack_sync_messages "
                    "    WHERE channel_id = $1 "
                    "      AND parent_message_ts = $2 "
                    "      AND NOT (message_ts = ANY($3::text[])) "
                    "    RETURNING 1"
                    ") "
                    "SELECT COUNT(*) FROM deleted",
                    channel_id,
                    thread_ts,
                    reply_ts_values,
                )
            else:
                deleted = await conn.fetchval(
                    "WITH deleted AS ("
                    "    DELETE FROM slack_sync_messages "
                    "    WHERE channel_id = $1 "
                    "      AND parent_message_ts = $2 "
                    "    RETURNING 1"
                    ") "
                    "SELECT COUNT(*) FROM deleted",
                    channel_id,
                    thread_ts,
                )
    return upserted, int(deleted or 0)


async def mark_thread_refreshed(
    pool,
    *,
    channel_id: str,
    thread_ts: str,
) -> None:
    """Mark the root row for one thread as freshly reconciled."""
    await pool.execute(
        "UPDATE slack_sync_messages SET "
        "thread_refreshed_at = NOW(), updated_at = NOW(), last_seen_at = NOW() "
        "WHERE channel_id = $1 "
        "  AND message_ts = $2 "
        "  AND is_thread_root = TRUE",
        channel_id,
        thread_ts,
    )


async def record_run_start(
    pool,
    *,
    run_id: str,
    workflow_run_id: str,
    mode: str,
    requested: list[dict[str, str]],
    skipped: list[dict[str, str]],
    metadata: dict[str, Any],
) -> None:
    """Insert or reset the ETL run row."""
    await pool.execute(
        "INSERT INTO slack_sync_runs ("
        "run_id, workflow_run_id, mode, status, channels_requested, channels_skipped, metadata"
        ") VALUES ($1, $2, $3, 'running', $4::jsonb, $5::jsonb, $6::jsonb) "
        "ON CONFLICT (run_id) DO UPDATE SET "
        "workflow_run_id = EXCLUDED.workflow_run_id, "
        "mode = EXCLUDED.mode, "
        "status = 'running', "
        "channels_requested = EXCLUDED.channels_requested, "
        "channels_synced = '[]'::jsonb, "
        "channels_skipped = EXCLUDED.channels_skipped, "
        "channels_failed = '[]'::jsonb, "
        "messages_fetched = 0, "
        "messages_upserted = 0, "
        "threads_fetched = 0, "
        "replies_fetched = 0, "
        "replies_upserted = 0, "
        "finished_at = NULL, "
        "error_text = '', "
        "metadata = EXCLUDED.metadata",
        run_id,
        workflow_run_id,
        mode,
        canonical_json(requested),
        canonical_json(skipped),
        canonical_json(metadata),
    )


async def record_run_finish(
    pool,
    *,
    run_id: str,
    status: str,
    synced: list[dict[str, str]],
    skipped: list[dict[str, str]],
    failed: list[dict[str, str]],
    counts: dict[str, int],
    error_text: str = "",
) -> None:
    """Finalize a sync run with channel outcomes and row counts."""
    await pool.execute(
        "UPDATE slack_sync_runs SET "
        "status = $2, channels_synced = $3::jsonb, channels_skipped = $4::jsonb, "
        "channels_failed = $5::jsonb, messages_fetched = $6, messages_upserted = $7, "
        "threads_fetched = $8, replies_fetched = $9, replies_upserted = $10, "
        "finished_at = NOW(), error_text = $11 "
        "WHERE run_id = $1",
        run_id,
        status,
        canonical_json(synced),
        canonical_json(skipped),
        canonical_json(failed),
        counts.get("messages_fetched", 0),
        counts.get("messages_upserted", 0),
        counts.get("threads_fetched", 0),
        counts.get("replies_fetched", 0),
        counts.get("replies_upserted", 0),
        error_text,
    )


def workflow_run_id_to_sync_run_id(workflow_run_id: str) -> str:
    """Derive a stable sync run id from the durable workflow run id."""
    safe_run_id = "".join(char if char.isalnum() else "_" for char in workflow_run_id)
    return f"slack_sync_{safe_run_id}"


class SlackEtlAuthError(RuntimeError):
    """Structured Slack ETL auth failure."""

    def __init__(
        self,
        *,
        slack_method: str,
        error_code: str,
        status_code: int | None,
        requested_channel: str | None = None,
        resolved_channel: str | None = None,
    ) -> None:
        payload = {
            "error": "slack_auth_failed",
            "message": f"Slack authentication failed for {slack_method} via user_token",
            "slack_method": slack_method,
            "access_path": "user_token",
            "error_code": error_code,
            "status_code": status_code,
            "requested_channel": requested_channel,
            "resolved_channel": resolved_channel,
        }
        self.payload = payload
        super().__init__(json.dumps(payload, sort_keys=True))


class SlackEtlRateLimitError(RuntimeError):
    """Structured Slack ETL rate-limit failure."""

    def __init__(self, *, slack_method: str, retry_after: float) -> None:
        payload = {
            "error": "slack_rate_limited",
            "message": f"Slack rate limited {slack_method}; retry after {retry_after:.2f}s",
            "slack_method": slack_method,
            "access_path": "user_token",
            "retry_after_seconds": retry_after,
        }
        self.payload = payload
        super().__init__(json.dumps(payload, sort_keys=True))


class SlackEtlClient:
    """Slack user-token client used only by Slack ETL workflows."""

    _MAX_PAGE_SIZE = 200
    _DEFAULT_API_TIMEOUT_SECONDS = 8
    _MAX_RATE_LIMIT_SLEEP_SECONDS = 30.0
    _DATE_ONLY_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
    _NUMERIC_TS_RE = re.compile(r"^\d+(?:\.\d+)?$")
    _CHANNEL_ID_RE = re.compile(r"^[CGD][A-Z0-9]+$")
    _AUTH_ERROR_CODES: ClassVar[frozenset[str]] = frozenset(
        {
            "account_inactive",
            "invalid_auth",
            "missing_scope",
            "no_permission",
            "not_allowed_token_type",
            "not_authed",
            "token_revoked",
        }
    )

    def __init__(self, etl_token: str | None = None) -> None:
        from slack_sdk import WebClient

        token = (etl_token or secret("SLACK_ETL_TOKEN", default="")).strip()
        if not token:
            raise RuntimeError("SLACK_ETL_TOKEN not set for Slack ETL workflow")
        self.token = token
        self._client = WebClient(token=token, timeout=self._api_timeout_seconds())
        self._user_cache: dict[str, str] = {}
        self._ratelimit_deadlines: dict[str, float] = {}

    @classmethod
    def _api_timeout_seconds(cls) -> int:
        raw = secret("SLACK_API_TIMEOUT_SECONDS", default="")
        if raw is None:
            return cls._DEFAULT_API_TIMEOUT_SECONDS
        raw = str(raw).strip()
        if not raw:
            return cls._DEFAULT_API_TIMEOUT_SECONDS
        try:
            return max(1, int(raw))
        except ValueError:
            return cls._DEFAULT_API_TIMEOUT_SECONDS

    def _is_ratelimit_error(self, error: Exception) -> bool:
        response = getattr(error, "response", None)
        status_code = getattr(response, "status_code", None)
        response_error = response.get("error") if hasattr(response, "get") else None
        return status_code == 429 or response_error == "ratelimited"

    def _slack_error_code(self, error: Exception) -> str:
        response = getattr(error, "response", None)
        if hasattr(response, "get"):
            return str(response.get("error") or "unknown_error")
        return "unknown_error"

    def _is_auth_error(self, error: Exception) -> bool:
        response = getattr(error, "response", None)
        status_code = getattr(response, "status_code", None)
        return (
            status_code in {401, 403}
            or self._slack_error_code(error) in self._AUTH_ERROR_CODES
        )

    def _raise_slack_api_error(
        self,
        error: Exception,
        *,
        slack_method: str,
        requested_channel: str | None = None,
        resolved_channel: str | None = None,
    ) -> None:
        error_code = self._slack_error_code(error)
        response = getattr(error, "response", None)
        status_code = getattr(response, "status_code", None)
        if self._is_auth_error(error):
            raise SlackEtlAuthError(
                slack_method=slack_method,
                error_code=error_code,
                status_code=status_code,
                requested_channel=requested_channel,
                resolved_channel=resolved_channel,
            ) from error
        raise RuntimeError(f"Slack API error: {error_code}") from error

    def _parse_retry_after(self, value: str | None, default: int = 5) -> float:
        try:
            seconds = float(value) if value is not None else float(default)
        except (TypeError, ValueError):
            seconds = float(default)
        return max(seconds, 1.0) + 0.25

    def _retry_on_ratelimit(
        self,
        func,
        *args,
        method_key: str | None = None,
        max_retries: int = 6,
        max_retry_sleep_s: float | None = None,
        **kwargs,
    ):
        key = method_key or getattr(func, "__name__", "slack_api_call")
        max_sleep = (
            self._MAX_RATE_LIMIT_SLEEP_SECONDS
            if max_retry_sleep_s is None
            else max(0.0, max_retry_sleep_s)
        )
        for attempt in range(max_retries):
            blocked_until = self._ratelimit_deadlines.get(key, 0.0)
            remaining = blocked_until - time.time()
            if remaining > 0:
                if remaining > max_sleep:
                    raise SlackEtlRateLimitError(
                        slack_method=key,
                        retry_after=round(remaining, 3),
                    )
                time.sleep(remaining)

            try:
                return func(*args, **kwargs)
            except Exception as exc:
                if self._is_ratelimit_error(exc):
                    retry_after = self._parse_retry_after(
                        getattr(getattr(exc, "response", None), "headers", {}).get(
                            "Retry-After"
                        ),
                        default=max(1, 2**attempt),
                    )
                    self._ratelimit_deadlines[key] = time.time() + retry_after
                    if attempt < max_retries - 1 and retry_after <= max_sleep:
                        time.sleep(retry_after)
                        continue
                    raise SlackEtlRateLimitError(
                        slack_method=key,
                        retry_after=retry_after,
                    ) from exc
                raise
        raise RuntimeError("Max retries exceeded")

    def _format_ts(self, value: float) -> str:
        return f"{value:.6f}"

    def _normalize_ts(self, value: str | int | float | None) -> str | None:
        if value in (None, ""):
            return None

        if isinstance(value, int | float):
            seconds = float(value)
            if seconds >= 1_000_000_000_000:
                seconds /= 1000.0
            return self._format_ts(seconds)

        raw = str(value).strip()
        if not raw:
            return None

        if self._NUMERIC_TS_RE.fullmatch(raw):
            seconds = float(raw)
            if "." not in raw and len(raw) >= 13:
                seconds /= 1000.0
            return self._format_ts(seconds)

        if self._DATE_ONLY_RE.fullmatch(raw):
            parsed = dt.datetime.fromisoformat(f"{raw}T00:00:00+00:00")
            return self._format_ts(parsed.timestamp())

        try:
            parsed = dt.datetime.fromisoformat(raw.replace("Z", "+00:00"))
        except ValueError as exc:
            raise ValueError(
                f"Unsupported timestamp format '{value}'. Use Slack ts, epoch seconds, ISO datetime, or YYYY-MM-DD."
            ) from exc

        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=dt.UTC)
        return self._format_ts(parsed.timestamp())

    def _message_permalink(self, channel_id: str, ts: str) -> str:
        return f"https://slack.com/archives/{channel_id}/p{ts.replace('.', '')}"

    def _resolve_mentions(self, text: str, user_cache: dict[str, str]) -> str:
        def replace_mention(match: re.Match) -> str:
            user_id = match.group(1)
            name = user_cache.get(user_id, user_id)
            return f"@{name}"

        return re.sub(r"<@([A-Z0-9]+)>", replace_mention, text)

    def _download_slack_file_bytes(
        self,
        url: str,
        *,
        max_bytes: int,
    ) -> tuple[str, bytes]:
        """Fetch one Slack file with the ETL token, enforcing the byte cap."""
        request = urllib_request.Request(
            url,
            headers={"Authorization": f"Bearer {self.token}"},
            method="GET",
        )
        with urllib_request.urlopen(
            request,
            timeout=self._api_timeout_seconds(),
        ) as response:
            body = response.read(max_bytes + 1)
            mime_type = response.headers.get_content_type()
        if len(body) > max_bytes:
            raise ValueError(
                f"slack file exceeds SLACK_ETL_ATTACHMENT_MAX_BYTES ({max_bytes} bytes)"
            )
        return mime_type, body

    def _serialize_file(self, file_obj: dict[str, Any]) -> dict[str, Any] | None:
        """Normalize Slack file metadata and optionally include downloaded bytes."""
        slack_file_id = str(file_obj.get("id") or "").strip()
        if not slack_file_id:
            return None

        name = str(file_obj.get("name") or "")
        url_private = str(
            file_obj.get("url_private_download") or file_obj.get("url_private") or ""
        )
        mimetype = str(file_obj.get("mimetype") or "")
        size_bytes = _safe_int(file_obj.get("size"))
        normalized: dict[str, Any] = {
            "id": slack_file_id,
            "name": name,
            "title": str(file_obj.get("title") or ""),
            "mimetype": mimetype,
            "filetype": str(file_obj.get("filetype") or ""),
            "size": size_bytes,
            "url_private": url_private,
            "permalink": str(file_obj.get("permalink") or ""),
            "download_status": "metadata_only",
            "download_error": "",
            "content_sha256": None,
            "content_bytes": None,
            "raw_payload": file_obj,
        }

        if not attachment_download_enabled():
            normalized["download_status"] = "disabled"
            return normalized
        if not url_private:
            normalized["download_status"] = "missing_url"
            return normalized

        max_bytes = attachment_max_bytes()
        if size_bytes > max_bytes:
            normalized["download_status"] = "skipped_too_large"
            normalized["download_error"] = (
                f"slack file size {size_bytes} exceeds SLACK_ETL_ATTACHMENT_MAX_BYTES "
                f"({max_bytes} bytes)"
            )
            return normalized

        try:
            response_mimetype, body = self._download_slack_file_bytes(
                url_private,
                max_bytes=max_bytes,
            )
        except (OSError, ValueError, urllib_error.URLError) as exc:
            normalized["download_status"] = "failed"
            normalized["download_error"] = str(exc)
            return normalized

        normalized["download_status"] = "downloaded"
        normalized["content_bytes"] = body
        normalized["content_sha256"] = hashlib.sha256(body).hexdigest()
        if not normalized["mimetype"]:
            normalized["mimetype"] = (
                response_mimetype
                or mimetypes.guess_type(name)[0]
                or "application/octet-stream"
            )
        if not normalized["size"]:
            normalized["size"] = len(body)
        return normalized

    def _serialize_message(
        self,
        msg: dict[str, Any],
        channel_id: str,
        user_cache: dict[str, str],
        *,
        channel_name: str | None = None,
    ) -> dict[str, Any]:
        user_id = msg.get("user") or msg.get("bot_id", "")
        username = user_cache.get(user_id, msg.get("username", user_id))
        if not username:
            username = msg.get("bot_profile", {}).get("name", "") or user_id

        ts = msg.get("ts", "")
        message = {
            "user": username,
            "user_id": user_id,
            "text": self._resolve_mentions(msg.get("text", ""), user_cache),
            "timestamp": ts,
            "permalink": self._message_permalink(channel_id, ts),
            "channel_id": channel_id,
            "thread_ts": msg.get("thread_ts"),
            "reply_count": msg.get("reply_count", 0),
            "reply_users": msg.get("reply_users", []),
            "latest_reply": msg.get("latest_reply"),
            "type": msg.get("type", "message"),
            "subtype": msg.get("subtype"),
            "parent_user_id": msg.get("parent_user_id"),
            "bot_id": msg.get("bot_id"),
            "files": [
                normalized
                for file_obj in msg.get("files", []) or []
                if isinstance(file_obj, dict)
                for normalized in [self._serialize_file(file_obj)]
                if normalized is not None
            ],
        }
        if channel_name is not None:
            message["channel"] = channel_name
        return message

    def _collect_cursor_pages(
        self,
        fetch_page: Callable[[str | None, int], dict[str, Any]],
        *,
        result_key: str,
        limit: int,
        cursor: str | None = None,
    ) -> tuple[list[dict[str, Any]], str | None, bool]:
        remaining = max(limit, 0)
        next_cursor = cursor
        items: list[dict[str, Any]] = []

        while remaining > 0:
            batch_limit = min(remaining, self._MAX_PAGE_SIZE)
            response = fetch_page(next_cursor, batch_limit)
            batch = response.get(result_key, []) or []
            items.extend(batch)

            next_cursor = (
                response.get("response_metadata", {}).get("next_cursor") or None
            )
            has_more = bool(next_cursor or response.get("has_more"))
            if not has_more or not batch:
                return items, next_cursor, has_more

            remaining = limit - len(items)

        return items, next_cursor, bool(next_cursor)

    def _clean_channel_ref(self, channel: str) -> str:
        raw = str(channel).strip()
        if raw.startswith("<#") and raw.endswith(">"):
            raw = raw[2:-1].split("|", 1)[0]
        return raw.lstrip("#").strip()

    def _looks_like_channel_id(self, channel: str) -> bool:
        return bool(
            self._CHANNEL_ID_RE.fullmatch(self._clean_channel_ref(channel).upper())
        )

    def _resolve_etl_channel(self, channel: str) -> str:
        normalized = self._clean_channel_ref(channel)
        if self._looks_like_channel_id(normalized):
            return normalized.upper()

        for item in self._list_etl_channels(limit=10_000):
            if item["name"] == normalized:
                return item["id"]
        raise RuntimeError(
            f"Channel '{channel}' not found through Slack ETL user token"
        )

    def _resolve_etl_channel_name(self, channel: str, channel_id: str) -> str:
        normalized = channel.lstrip("#")
        if normalized != channel_id:
            return normalized
        return channel_id

    def _etl_access_mode(self) -> str:
        return "user_token"

    def _get_etl_user_cache(self) -> dict[str, str]:
        if self._user_cache:
            return self._user_cache

        user_cache: dict[str, str] = {}
        try:
            users_response = self._retry_on_ratelimit(
                self._client.users_list,
                method_key="etl.users.list",
                limit=1000,
            )
            for user in users_response.get("members", []):
                user_cache[user.get("id", "")] = user.get("name", "")
            self._user_cache = user_cache
        except SlackEtlRateLimitError:
            pass
        except Exception:
            pass
        return user_cache

    def _list_etl_channels(
        self,
        limit: int = 500,
        force_refresh: bool = False,
    ) -> list[dict]:
        channels = []
        cursor = None

        while len(channels) < limit:
            try:
                response = self._retry_on_ratelimit(
                    self._client.conversations_list,
                    method_key="etl.conversations.list",
                    types="public_channel",
                    limit=min(limit - len(channels), self._MAX_PAGE_SIZE),
                    cursor=cursor,
                    exclude_archived=True,
                )
            except SlackEtlRateLimitError:
                raise
            except Exception as exc:
                self._raise_slack_api_error(
                    exc,
                    slack_method="conversations.list",
                )

            for channel in response.get("channels", []):
                if channel.get("is_private", False):
                    continue
                channels.append(
                    {
                        "id": channel.get("id", ""),
                        "name": channel.get("name", ""),
                        "purpose": channel.get("purpose", {}).get("value", ""),
                        "topic": channel.get("topic", {}).get("value", ""),
                        "member_count": channel.get("num_members", 0),
                        "is_archived": channel.get("is_archived", False),
                        "is_private": channel.get("is_private", False),
                        "is_member": channel.get("is_member", False),
                    }
                )

            cursor = response.get("response_metadata", {}).get("next_cursor")
            if not cursor:
                break

        return sorted(channels[:limit], key=lambda x: x["name"])

    def _get_etl_channel_history_page(
        self,
        channel: str,
        limit: int = 50,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = False,
    ) -> dict[str, Any]:
        user_cache = self._get_etl_user_cache()
        channel_id = self._resolve_etl_channel(channel)
        channel_name = self._resolve_etl_channel_name(channel, channel_id)
        normalized_oldest = self._normalize_ts(oldest)
        normalized_latest = self._normalize_ts(latest)

        requested_limit = max(1, min(int(limit), self._MAX_PAGE_SIZE))

        def fetch_page(next_cursor: str | None, batch_limit: int) -> dict[str, Any]:
            kwargs: dict[str, Any] = {
                "channel": channel_id,
                "limit": batch_limit,
            }
            if next_cursor:
                kwargs["cursor"] = next_cursor
            if normalized_oldest is not None:
                kwargs["oldest"] = normalized_oldest
            if normalized_latest is not None:
                kwargs["latest"] = normalized_latest
            if normalized_oldest is not None or normalized_latest is not None:
                kwargs["inclusive"] = inclusive
            return self._retry_on_ratelimit(
                self._client.conversations_history,
                method_key="etl.conversations.history",
                **kwargs,
            )

        try:
            raw_messages, next_cursor, has_more = self._collect_cursor_pages(
                fetch_page,
                result_key="messages",
                limit=requested_limit,
                cursor=cursor,
            )
        except SlackEtlRateLimitError:
            raise
        except Exception as exc:
            self._raise_slack_api_error(
                exc,
                slack_method="conversations.history",
                requested_channel=channel,
                resolved_channel=channel_id,
            )

        messages = [
            self._serialize_message(msg, channel_id, user_cache) for msg in raw_messages
        ]

        return {
            "channel": channel_name,
            "channel_id": channel_id,
            "messages": messages,
            "count": len(messages),
            "has_more": has_more,
            "next_cursor": next_cursor,
            "window": {
                "oldest": normalized_oldest,
                "latest": normalized_latest,
                "inclusive": inclusive,
            },
            "order": "desc",
        }

    def _get_etl_thread_replies_page(
        self,
        channel: str,
        thread_ts: str,
        limit: int = 50,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = True,
    ) -> dict[str, Any]:
        user_cache = self._get_etl_user_cache()
        channel_id = self._resolve_etl_channel(channel)
        normalized_oldest = self._normalize_ts(oldest)
        normalized_latest = self._normalize_ts(latest)
        normalized_thread_ts = self._normalize_ts(thread_ts)

        if normalized_thread_ts is None:
            raise ValueError("thread_ts is required")

        requested_limit = max(1, min(int(limit), self._MAX_PAGE_SIZE))

        def fetch_page(next_cursor: str | None, batch_limit: int) -> dict[str, Any]:
            kwargs: dict[str, Any] = {
                "channel": channel_id,
                "ts": normalized_thread_ts,
                "limit": batch_limit,
                "inclusive": inclusive,
            }
            if next_cursor:
                kwargs["cursor"] = next_cursor
            if normalized_oldest is not None:
                kwargs["oldest"] = normalized_oldest
            if normalized_latest is not None:
                kwargs["latest"] = normalized_latest
            return self._retry_on_ratelimit(
                self._client.conversations_replies,
                method_key="etl.conversations.replies",
                **kwargs,
            )

        try:
            raw_messages, next_cursor, has_more = self._collect_cursor_pages(
                fetch_page,
                result_key="messages",
                limit=requested_limit,
                cursor=cursor,
            )
        except SlackEtlRateLimitError:
            raise
        except Exception as exc:
            self._raise_slack_api_error(
                exc,
                slack_method="conversations.replies",
                requested_channel=channel,
                resolved_channel=channel_id,
            )

        messages = [
            self._serialize_message(msg, channel_id, user_cache) for msg in raw_messages
        ]

        return {
            "channel_id": channel_id,
            "thread_ts": normalized_thread_ts,
            "messages": messages,
            "count": len(messages),
            "requested_limit": limit,
            "effective_limit": requested_limit,
            "has_more": has_more,
            "next_cursor": next_cursor,
            "continuation_available": has_more,
            "window": {
                "oldest": normalized_oldest,
                "latest": normalized_latest,
                "inclusive": inclusive,
            },
            "order": "asc",
        }

    def _sync_etl_channel_history(
        self,
        channel: str,
        state: dict[str, Any] | None = None,
        limit: int = 200,
        lookback_days: int = 30,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
    ) -> dict[str, Any]:
        sync_state = dict(state or {})
        cursor = sync_state.get("cursor")
        watermark = self._normalize_ts(sync_state.get("watermark"))
        normalized_oldest = self._normalize_ts(oldest) or sync_state.get("oldest")
        normalized_latest = self._normalize_ts(latest) or sync_state.get("latest")

        if cursor is None and normalized_oldest is None:
            if watermark is not None:
                lookback_seconds = max(lookback_days, 0) * 86400
                normalized_oldest = self._format_ts(
                    max(float(watermark) - lookback_seconds, 0.0)
                )
            elif lookback_days > 0:
                normalized_oldest = self._format_ts(
                    max(time.time() - (lookback_days * 86400), 0.0)
                )

        page = self._get_etl_channel_history_page(
            channel=channel,
            limit=limit,
            cursor=cursor,
            oldest=normalized_oldest,
            latest=normalized_latest,
            inclusive=True,
        )

        latest_seen = watermark
        if page["messages"]:
            latest_seen = self._format_ts(
                max(float(message["timestamp"]) for message in page["messages"])
            )

        next_state: dict[str, Any] = {
            "cursor": page["next_cursor"] if page["has_more"] else None,
            "watermark": latest_seen or watermark,
            "lookback_days": lookback_days,
            "oldest": page["window"]["oldest"] if page["has_more"] else None,
            "latest": page["window"]["latest"] if page["has_more"] else None,
        }

        return {
            **page,
            "sync_state": next_state,
        }

    def _list_etl_users(self, limit: int = 200) -> list[dict]:
        users = []
        cursor = None

        while len(users) < limit:
            try:
                kwargs: dict[str, Any] = {
                    "limit": min(limit - len(users), self._MAX_PAGE_SIZE),
                }
                if cursor:
                    kwargs["cursor"] = cursor
                response = self._retry_on_ratelimit(
                    self._client.users_list,
                    method_key="etl.users.list",
                    **kwargs,
                )
            except SlackEtlRateLimitError:
                raise
            except Exception as exc:
                self._raise_slack_api_error(
                    exc,
                    slack_method="users.list",
                )

            for user in response.get("members", []):
                if user.get("deleted"):
                    continue
                profile = user.get("profile", {}) or {}
                users.append(
                    {
                        "id": user.get("id", ""),
                        "name": user.get("name", ""),
                        "real_name": user.get("real_name", ""),
                        "display_name": profile.get("display_name", ""),
                        "email": profile.get("email", ""),
                        "title": profile.get("title", ""),
                        "is_bot": user.get("is_bot", False),
                        "is_deleted": user.get("deleted", False),
                        "team_id": user.get("team_id", "") or user.get("team", ""),
                    }
                )

            cursor = response.get("response_metadata", {}).get("next_cursor")
            if not cursor:
                break

        return sorted(users[:limit], key=lambda x: x["name"])


def client() -> SlackSyncClient:
    """Construct the workflow-owned Slack ETL client."""
    return SlackEtlClient()


async def enqueue_backfill_job(
    pool,
    *,
    job_key: str,
    job_type: str,
    channel_id: str,
    payload: dict[str, Any],
    run_id: str,
    priority: int = 100,
    refresh_completed: bool = True,
) -> None:
    """Store or refresh a queued backfill job outside the incremental checkpoint."""
    if not payload:
        return
    completion_guard = (
        ""
        if refresh_completed
        else " WHERE slack_sync_backfill_jobs.status <> 'completed'"
    )
    await pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, status, payload_json, "
        "priority, last_run_id, last_enqueued_at, last_error, updated_at"
        ") VALUES ($1, $2, $3, $4, 'pending', $5::jsonb, $6, $7, NOW(), '', NOW()) "
        "ON CONFLICT (job_key) DO UPDATE SET "
        "job_type = EXCLUDED.job_type, "
        "payload_version = EXCLUDED.payload_version, "
        "channel_id = EXCLUDED.channel_id, "
        "status = 'pending', "
        "payload_json = EXCLUDED.payload_json, "
        "priority = EXCLUDED.priority, "
        "attempt_count = CASE "
        "    WHEN slack_sync_backfill_jobs.status = 'running' THEN slack_sync_backfill_jobs.attempt_count "
        "    ELSE 0 "
        "END, "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_enqueued_at = NOW(), "
        "last_completed_at = NULL, "
        "last_error = '', "
        "updated_at = NOW()" + completion_guard,
        job_key,
        job_type,
        BACKFILL_JOB_PAYLOAD_VERSION,
        channel_id,
        canonical_json(payload),
        priority,
        run_id,
    )


async def seed_channel_bootstrap_job(
    pool,
    *,
    channel_id: str,
    window_oldest: str,
    window_latest: str,
    lookback_days: int,
    thread_lookback_days: int,
    run_id: str,
    priority: int = 200,
) -> bool:
    """Create the one initial historical bootstrap job for a channel.

    Bootstrap rows are channel state, not per-sync-run events: once a row exists,
    incremental sync must leave its fixed window and cursor progress alone.
    """
    if not channel_id or not window_oldest or not window_latest:
        return False
    result = await pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, status, payload_json, "
        "priority, last_run_id, last_enqueued_at, last_error, updated_at"
        ") VALUES ($1, $2, $3, $4, 'pending', $5::jsonb, $6, $7, NOW(), '', NOW()) "
        "ON CONFLICT DO NOTHING",
        f"bootstrap:{channel_id}",
        BACKFILL_JOB_CHANNEL_BOOTSTRAP,
        BACKFILL_JOB_PAYLOAD_VERSION,
        channel_id,
        canonical_json(
            {
                "cursor": None,
                "window_oldest": window_oldest,
                "window_latest": window_latest,
                "lookback_days": lookback_days,
                "thread_lookback_days": thread_lookback_days,
            }
        ),
        priority,
        run_id,
    )
    return result == "INSERT 0 1"


async def widen_channel_bootstrap_job(
    pool,
    *,
    channel_id: str,
    window_oldest: str,
    lookback_days: int,
    thread_lookback_days: int,
    run_id: str,
    priority: int = 150,
) -> bool:
    """Reopen or widen a bootstrap job when the configured lookback increases.

    The update is intentionally limited to jobs without an active cursor. Running
    or partially drained jobs keep their original Slack cursor/window until the
    next incremental sync can safely revisit them.
    """
    if not channel_id or not window_oldest:
        return False
    result = await pool.execute(
        "UPDATE slack_sync_backfill_jobs "
        "SET status = 'pending', "
        "    payload_json = jsonb_build_object("
        "        'cursor', NULL, "
        "        'window_oldest', $2::text, "
        "        'window_latest', CASE "
        "            WHEN status = 'completed' THEN payload_json->>'window_oldest' "
        "            ELSE payload_json->>'window_latest' "
        "        END, "
        "        'lookback_days', $7::int, "
        "        'thread_lookback_days', $8::int"
        "    ), "
        "    priority = $3, "
        "    attempt_count = 0, "
        "    last_run_id = $4, "
        "    last_enqueued_at = NOW(), "
        "    last_started_at = NULL, "
        "    last_completed_at = NULL, "
        "    last_error = '', "
        "    updated_at = NOW() "
        "WHERE job_key = $1 "
        "  AND job_type = $5 "
        "  AND payload_version = $6 "
        "  AND (payload_json->>'lookback_days')::int < $7 "
        "  AND COALESCE(payload_json->>'window_oldest', '') <> '' "
        "  AND COALESCE(payload_json->>'cursor', '') = '' "
        "  AND status IN ('pending', 'failed', 'completed')",
        f"bootstrap:{channel_id}",
        window_oldest,
        priority,
        run_id,
        BACKFILL_JOB_CHANNEL_BOOTSTRAP,
        BACKFILL_JOB_PAYLOAD_VERSION,
        lookback_days,
        thread_lookback_days,
    )
    return result == "UPDATE 1"


async def claim_backfill_jobs(pool, limit: int) -> list[dict[str, Any]]:
    """Claim a bounded batch of pending backfill jobs for one workflow run."""
    async with pool.acquire() as conn:
        async with conn.transaction():
            rows = await conn.fetch(
                "WITH claimed AS ("
                "    SELECT job_id "
                "    FROM slack_sync_backfill_jobs "
                "    WHERE status IN ('pending', 'failed') "
                "    ORDER BY priority, updated_at, job_id "
                "    LIMIT $1 "
                "    FOR UPDATE SKIP LOCKED"
                ") "
                "UPDATE slack_sync_backfill_jobs backfills "
                "SET status = 'running', "
                "    attempt_count = backfills.attempt_count + 1, "
                "    last_started_at = NOW(), "
                "    updated_at = NOW() "
                "FROM claimed "
                "WHERE backfills.job_id = claimed.job_id "
                "RETURNING backfills.job_id, backfills.job_key, backfills.job_type, "
                "backfills.payload_version, backfills.channel_id, backfills.payload_json, "
                "backfills.priority, backfills.attempt_count",
                limit,
            )
    return [dict(row) for row in rows]


async def load_backfill_job_metrics(pool) -> list[dict[str, Any]]:
    """Summarize Slack backfill queue state for dashboard gauges."""
    rows = await pool.fetch(
        "SELECT job_type, status, COUNT(*) AS job_count, "
        "COALESCE("
        "  EXTRACT(EPOCH FROM NOW() - MIN("
        "    CASE status "
        "      WHEN 'pending' THEN COALESCE(last_enqueued_at, updated_at, created_at) "
        "      WHEN 'running' THEN COALESCE(last_started_at, updated_at, created_at) "
        "      WHEN 'completed' THEN COALESCE(last_completed_at, updated_at, created_at) "
        "      ELSE COALESCE(updated_at, last_enqueued_at, created_at) "
        "    END"
        "  )), "
        "  0"
        ") AS oldest_age_seconds "
        "FROM slack_sync_backfill_jobs "
        "GROUP BY job_type, status",
    )
    summary = {
        (job_type, status): {"job_count": 0, "oldest_age_seconds": 0.0}
        for job_type in BACKFILL_JOB_TYPES
        for status in BACKFILL_JOB_STATUSES
    }
    for row in rows:
        job_type = str(row["job_type"] or "")
        status = str(row["status"] or "")
        if not job_type or not status:
            continue
        summary[(job_type, status)] = {
            "job_count": int(row["job_count"] or 0),
            "oldest_age_seconds": float(row["oldest_age_seconds"] or 0.0),
        }

    return [
        {
            "job_type": job_type,
            "status": status,
            **values,
        }
        for (job_type, status), values in sorted(summary.items())
    ]


async def mark_backfill_job_failed(
    pool,
    *,
    job_id: int,
    run_id: str,
    error: str,
) -> None:
    """Return a claimed backfill job to the queue as failed."""
    await pool.execute(
        "UPDATE slack_sync_backfill_jobs SET "
        "status = 'failed', last_run_id = $2, last_error = $3, updated_at = NOW() "
        "WHERE job_id = $1",
        job_id,
        run_id,
        error,
    )


async def mark_backfill_job_terminal_skipped(
    pool,
    *,
    job_id: int,
    run_id: str,
    error: str,
) -> None:
    """Mark a permanently unrefreshable backfill job terminal without retrying."""
    await pool.execute(
        "UPDATE slack_sync_backfill_jobs SET "
        "status = 'completed', "
        "last_run_id = $2, "
        "payload_json = COALESCE(payload_json, '{}'::jsonb) || jsonb_build_object("
        "    'terminal_skip_reason', $3::text, "
        "    'terminal_skip_at', NOW()::text"
        "), "
        "last_completed_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW() "
        "WHERE job_id = $1",
        job_id,
        run_id,
        error,
    )


async def mark_backfill_job_completed(
    pool,
    *,
    job_id: int,
    run_id: str,
    payload: dict[str, Any] | None = None,
) -> None:
    """Mark a finished backfill job as completed for observability and auditability."""
    if payload is None:
        await pool.execute(
            "UPDATE slack_sync_backfill_jobs SET "
            "status = 'completed', last_run_id = $2, last_completed_at = NOW(), "
            "last_error = '', updated_at = NOW() "
            "WHERE job_id = $1",
            job_id,
            run_id,
        )
        return
    await pool.execute(
        "UPDATE slack_sync_backfill_jobs SET "
        "status = 'completed', last_run_id = $2, payload_json = $3::jsonb, "
        "last_completed_at = NOW(), last_error = '', updated_at = NOW() "
        "WHERE job_id = $1",
        job_id,
        run_id,
        canonical_json(payload),
    )
