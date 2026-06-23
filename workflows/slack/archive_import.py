"""Workflow: import a public-channel Slack export archive into Slack ETL tables."""

from __future__ import annotations

import asyncio
import datetime as dt
import json
import os
import tempfile
import urllib.parse
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from api.runtime_control import canonical_json
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import record_run_finish, record_run_start

WORKFLOW_NAME = "slack_archive_import"

ARCHIVE_IMPORT_MODE = "archive_import"
MESSAGE_BATCH_SIZE = 1_000
ATTACHMENT_BATCH_SIZE = 1_000


@dataclass
class Input:
    """Runtime options for importing an uploaded Slack export archive."""

    import_id: str


def _as_text(value: Any) -> str:
    return str(value or "")


def _topic_value(value: Any) -> str:
    if isinstance(value, dict):
        return _as_text(value.get("value"))
    return _as_text(value)


def _safe_int(value: Any, default: int = 0) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    return parsed if parsed >= 0 else default


def _slack_ts_to_datetime(ts: str | None) -> dt.datetime | None:
    if not ts:
        return None
    try:
        return dt.datetime.fromtimestamp(float(ts), tz=dt.timezone.utc)
    except (TypeError, ValueError, OSError):
        return None


def _strip_sensitive_url_query(value: str) -> str:
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme not in {"http", "https"}:
        return value
    if parsed.netloc.endswith("slack.com") or parsed.netloc.endswith("slack-edge.com"):
        return urllib.parse.urlunsplit(
            (parsed.scheme, parsed.netloc, parsed.path, "", "")
        )
    return value


def _scrub_archive_payload(value: Any) -> Any:
    if isinstance(value, dict):
        return {str(key): _scrub_archive_payload(child) for key, child in value.items()}
    if isinstance(value, list):
        return [_scrub_archive_payload(child) for child in value]
    if isinstance(value, str):
        return _strip_sensitive_url_query(value)
    return value


def _message_ts(message: dict[str, Any]) -> str:
    if message.get("subtype") in {"message_changed", "message_deleted"}:
        original = message.get("original")
        if isinstance(original, dict) and original.get("ts"):
            return _as_text(original.get("ts"))
    return _as_text(message.get("ts"))


def _message_thread_ts(message: dict[str, Any], message_ts: str) -> str | None:
    raw = _as_text(message.get("thread_ts"))
    if raw and raw != "0000000000.000000":
        return raw
    original = message.get("original")
    if isinstance(original, dict):
        raw = _as_text(original.get("thread_ts"))
        if raw and raw != "0000000000.000000":
            return raw
    return message_ts if message.get("reply_count") else None


def _archive_message_row(
    channel_id: str,
    message: dict[str, Any],
    *,
    run_id: str,
    archive_file: str,
) -> dict[str, Any] | None:
    subtype = _as_text(message.get("subtype"))
    if subtype == "message_deleted":
        return None

    message_ts = _message_ts(message)
    if not message_ts:
        return None

    thread_ts = _message_thread_ts(message, message_ts)
    parent_message_ts = thread_ts if thread_ts and thread_ts != message_ts else None
    raw_payload = _scrub_archive_payload(message)
    if isinstance(raw_payload, dict):
        raw_payload.setdefault(
            "archive_import",
            {
                "source": "slack_archive",
                "archive_file": archive_file,
                "original_event_ts": _as_text(message.get("ts")),
            },
        )

    return {
        "channel_id": channel_id,
        "message_ts": message_ts,
        "occurred_at": _slack_ts_to_datetime(message_ts),
        "thread_ts": thread_ts,
        "parent_message_ts": parent_message_ts,
        "is_thread_root": bool(thread_ts and thread_ts == message_ts),
        "user_id": _as_text(message.get("user")),
        "bot_id": _as_text(message.get("bot_id")),
        "message_type": _as_text(message.get("type")) or "message",
        "message_subtype": subtype or None,
        "text": _as_text(message.get("text")),
        "permalink": _as_text(message.get("permalink")),
        "reply_count": _safe_int(message.get("reply_count")),
        "reply_users": message.get("reply_users") or [],
        "latest_reply_ts": message.get("latest_reply"),
        "raw_payload": raw_payload,
        "attachments": message.get("files") or [],
        "source_run_id": run_id,
    }


def _attachment_rows(row: dict[str, Any]) -> list[dict[str, Any]]:
    attachments = row.get("attachments")
    if not isinstance(attachments, list):
        return []

    result = []
    for attachment in attachments:
        if not isinstance(attachment, dict):
            continue
        slack_file_id = _as_text(attachment.get("id")).strip()
        if not slack_file_id:
            continue
        raw_payload = _scrub_archive_payload(attachment)
        result.append(
            {
                "channel_id": row["channel_id"],
                "message_ts": row["message_ts"],
                "slack_file_id": slack_file_id,
                "name": _as_text(attachment.get("name")),
                "title": _as_text(attachment.get("title")),
                "mimetype": _as_text(attachment.get("mimetype")),
                "filetype": _as_text(attachment.get("filetype")),
                "size_bytes": _safe_int(attachment.get("size")),
                "url_private": _as_text(raw_payload.get("url_private"))
                if isinstance(raw_payload, dict)
                else "",
                "permalink": _as_text(attachment.get("permalink")),
                "download_status": "metadata_only",
                "download_error": "",
                "content_sha256": None,
                "content_bytes": None,
                "raw_payload": raw_payload,
                "source_run_id": row["source_run_id"],
            }
        )
    return result


def _json_member(zip_file: zipfile.ZipFile, name: str) -> Any:
    try:
        with zip_file.open(name) as handle:
            return json.load(handle)
    except KeyError as exc:
        raise RuntimeError(f"Slack archive missing required {name}") from exc


def _message_files(zip_file: zipfile.ZipFile) -> Iterable[str]:
    for info in zip_file.infolist():
        name = info.filename
        if info.is_dir() or "/" not in name or not name.endswith(".json"):
            continue
        yield name


def _archive_channel_refs(channels: list[dict[str, Any]]) -> list[dict[str, str]]:
    refs = []
    for channel in channels:
        channel_id = _as_text(channel.get("id"))
        if not channel_id:
            continue
        refs.append(
            {
                "channel_id": channel_id,
                "channel_name": _as_text(channel.get("name")),
            }
        )
    return refs


async def _load_archive_import(pool, import_id: str) -> dict[str, Any]:
    row = await pool.fetchrow(
        "SELECT import_id, archive_uri, object_bucket, object_key, "
        "status, workflow_run_id, workflow_task_id FROM slack_archive_imports "
        "WHERE import_id = $1",
        import_id,
    )
    if row is None:
        raise RuntimeError(f"archive import not found: {import_id}")
    return dict(row)


async def _mark_import_running(pool, *, import_id: str, run_id: str) -> None:
    await pool.execute(
        "UPDATE slack_archive_imports SET status = 'importing', started_at = NOW(), "
        "error_text = '', updated_at = NOW(), workflow_run_id = COALESCE(workflow_run_id, $2) "
        "WHERE import_id = $1",
        import_id,
        run_id,
    )


async def _mark_import_completed(
    pool,
    *,
    import_id: str,
    counts: dict[str, int],
) -> None:
    await pool.execute(
        "UPDATE slack_archive_imports SET status = 'completed', finished_at = NOW(), "
        "channels_imported = $2, users_imported = $3, messages_imported = $4, "
        "error_text = '', updated_at = NOW() WHERE import_id = $1",
        import_id,
        counts.get("channels_imported", 0),
        counts.get("users_imported", 0),
        counts.get("messages_imported", 0),
    )


async def _mark_import_failed(pool, *, import_id: str, error_text: str) -> None:
    await pool.execute(
        "UPDATE slack_archive_imports SET status = 'failed', finished_at = NOW(), "
        "error_text = $2, updated_at = NOW() WHERE import_id = $1",
        import_id,
        error_text[:4000],
    )


async def _upsert_archive_channels(pool, channels: list[dict[str, Any]]) -> int:
    rows = []
    for channel in channels:
        channel_id = _as_text(channel.get("id"))
        if not channel_id:
            continue
        rows.append(
            (
                channel_id,
                _as_text(channel.get("name")),
                bool(channel.get("is_archived")),
                _topic_value(channel.get("topic")),
                _topic_value(channel.get("purpose")),
                len(channel.get("members") or []),
                canonical_json(_scrub_archive_payload(channel)),
            )
        )

    if not rows:
        return 0

    async with pool.acquire() as conn:
        await conn.executemany(
            "INSERT INTO slack_sync_channels ("
            "channel_id, channel_name, is_archived, is_syncable, topic, purpose, "
            "member_count, raw_payload, last_seen_at, updated_at"
            ") VALUES ($1, $2, $3, FALSE, $4, $5, $6, $7::jsonb, NOW(), NOW()) "
            "ON CONFLICT (channel_id) DO UPDATE SET "
            "channel_name = CASE WHEN slack_sync_channels.channel_name = '' "
            "THEN EXCLUDED.channel_name ELSE slack_sync_channels.channel_name END, "
            "topic = CASE WHEN slack_sync_channels.topic = '' "
            "THEN EXCLUDED.topic ELSE slack_sync_channels.topic END, "
            "purpose = CASE WHEN slack_sync_channels.purpose = '' "
            "THEN EXCLUDED.purpose ELSE slack_sync_channels.purpose END, "
            "member_count = CASE WHEN slack_sync_channels.member_count = 0 "
            "THEN EXCLUDED.member_count ELSE slack_sync_channels.member_count END, "
            "raw_payload = CASE WHEN slack_sync_channels.raw_payload = '{}'::jsonb "
            "THEN EXCLUDED.raw_payload ELSE slack_sync_channels.raw_payload END, "
            "last_seen_at = NOW(), updated_at = NOW()",
            rows,
        )
    return len(rows)


async def _upsert_archive_users(pool, users: list[dict[str, Any]]) -> int:
    rows = []
    for user in users:
        user_id = _as_text(user.get("id"))
        if not user_id:
            continue
        profile = user.get("profile") if isinstance(user.get("profile"), dict) else {}
        rows.append(
            (
                user_id,
                _as_text(user.get("name")),
                _as_text(user.get("real_name")),
                _as_text(user.get("display_name") or profile.get("display_name")),
                bool(user.get("is_bot")),
                bool(user.get("deleted") or user.get("is_deleted")),
                _as_text(user.get("team_id") or user.get("team")),
                canonical_json(_scrub_archive_payload(user)),
            )
        )

    if not rows:
        return 0

    async with pool.acquire() as conn:
        await conn.executemany(
            "INSERT INTO slack_sync_users ("
            "user_id, user_name, real_name, display_name, is_bot, is_deleted, "
            "team_id, raw_payload, last_seen_at, updated_at"
            ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, NOW(), NOW()) "
            "ON CONFLICT (user_id) DO UPDATE SET "
            "user_name = CASE WHEN slack_sync_users.user_name = '' "
            "THEN EXCLUDED.user_name ELSE slack_sync_users.user_name END, "
            "real_name = CASE WHEN slack_sync_users.real_name = '' "
            "THEN EXCLUDED.real_name ELSE slack_sync_users.real_name END, "
            "display_name = CASE WHEN slack_sync_users.display_name = '' "
            "THEN EXCLUDED.display_name ELSE slack_sync_users.display_name END, "
            "team_id = CASE WHEN slack_sync_users.team_id = '' "
            "THEN EXCLUDED.team_id ELSE slack_sync_users.team_id END, "
            "raw_payload = CASE WHEN slack_sync_users.raw_payload = '{}'::jsonb "
            "THEN EXCLUDED.raw_payload ELSE slack_sync_users.raw_payload END, "
            "last_seen_at = NOW(), updated_at = NOW()",
            rows,
        )
    return len(rows)


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
    "occurred_at = COALESCE(slack_sync_messages.occurred_at, EXCLUDED.occurred_at), "
    "thread_ts = COALESCE(slack_sync_messages.thread_ts, EXCLUDED.thread_ts), "
    "parent_message_ts = COALESCE("
    "slack_sync_messages.parent_message_ts, EXCLUDED.parent_message_ts"
    "), "
    "is_thread_root = slack_sync_messages.is_thread_root OR EXCLUDED.is_thread_root, "
    "user_id = CASE WHEN slack_sync_messages.user_id = '' "
    "THEN EXCLUDED.user_id ELSE slack_sync_messages.user_id END, "
    "bot_id = CASE WHEN slack_sync_messages.bot_id = '' "
    "THEN EXCLUDED.bot_id ELSE slack_sync_messages.bot_id END, "
    "message_type = CASE WHEN slack_sync_messages.message_type = '' "
    "THEN EXCLUDED.message_type ELSE slack_sync_messages.message_type END, "
    "message_subtype = COALESCE("
    "slack_sync_messages.message_subtype, EXCLUDED.message_subtype"
    "), "
    "text = CASE WHEN slack_sync_messages.text = '' "
    "THEN EXCLUDED.text ELSE slack_sync_messages.text END, "
    "permalink = CASE WHEN slack_sync_messages.permalink = '' "
    "THEN EXCLUDED.permalink ELSE slack_sync_messages.permalink END, "
    "reply_count = GREATEST(slack_sync_messages.reply_count, EXCLUDED.reply_count), "
    "reply_users = CASE WHEN slack_sync_messages.reply_users = '[]'::jsonb "
    "THEN EXCLUDED.reply_users ELSE slack_sync_messages.reply_users END, "
    "latest_reply_ts = COALESCE(slack_sync_messages.latest_reply_ts, EXCLUDED.latest_reply_ts), "
    "raw_payload = CASE WHEN slack_sync_messages.raw_payload = '{}'::jsonb "
    "THEN EXCLUDED.raw_payload ELSE slack_sync_messages.raw_payload END, "
    "source_run_id = COALESCE(slack_sync_messages.source_run_id, EXCLUDED.source_run_id), "
    "last_seen_at = NOW(), updated_at = NOW()"
)


def _message_params(row: dict[str, Any]) -> tuple[Any, ...]:
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


_ATTACHMENT_UPSERT_SQL = (
    "INSERT INTO slack_sync_message_attachments ("
    "channel_id, message_ts, slack_file_id, name, title, mimetype, filetype, "
    "size_bytes, url_private, permalink, download_status, download_error, "
    "content_sha256, content_bytes, raw_payload, source_run_id, last_seen_at, updated_at"
    ") VALUES ("
    "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, "
    "$15::jsonb, $16, NOW(), NOW()"
    ") ON CONFLICT (channel_id, message_ts, slack_file_id) DO UPDATE SET "
    "name = CASE WHEN slack_sync_message_attachments.name = '' "
    "THEN EXCLUDED.name ELSE slack_sync_message_attachments.name END, "
    "title = CASE WHEN slack_sync_message_attachments.title = '' "
    "THEN EXCLUDED.title ELSE slack_sync_message_attachments.title END, "
    "mimetype = CASE WHEN slack_sync_message_attachments.mimetype = '' "
    "THEN EXCLUDED.mimetype ELSE slack_sync_message_attachments.mimetype END, "
    "filetype = CASE WHEN slack_sync_message_attachments.filetype = '' "
    "THEN EXCLUDED.filetype ELSE slack_sync_message_attachments.filetype END, "
    "size_bytes = CASE WHEN slack_sync_message_attachments.size_bytes = 0 "
    "THEN EXCLUDED.size_bytes ELSE slack_sync_message_attachments.size_bytes END, "
    "url_private = CASE WHEN slack_sync_message_attachments.url_private = '' "
    "THEN EXCLUDED.url_private ELSE slack_sync_message_attachments.url_private END, "
    "permalink = CASE WHEN slack_sync_message_attachments.permalink = '' "
    "THEN EXCLUDED.permalink ELSE slack_sync_message_attachments.permalink END, "
    "download_status = CASE WHEN slack_sync_message_attachments.content_bytes IS NULL "
    "AND slack_sync_message_attachments.download_status IN ('', 'metadata_only') "
    "THEN EXCLUDED.download_status ELSE slack_sync_message_attachments.download_status END, "
    "download_error = CASE WHEN slack_sync_message_attachments.download_error = '' "
    "THEN EXCLUDED.download_error ELSE slack_sync_message_attachments.download_error END, "
    "raw_payload = CASE WHEN slack_sync_message_attachments.raw_payload = '{}'::jsonb "
    "THEN EXCLUDED.raw_payload ELSE slack_sync_message_attachments.raw_payload END, "
    "source_run_id = COALESCE("
    "slack_sync_message_attachments.source_run_id, EXCLUDED.source_run_id"
    "), "
    "last_seen_at = NOW(), updated_at = NOW()"
)


def _attachment_params(row: dict[str, Any]) -> tuple[Any, ...]:
    return (
        row["channel_id"],
        row["message_ts"],
        row["slack_file_id"],
        row["name"],
        row["title"],
        row["mimetype"],
        row["filetype"],
        row["size_bytes"],
        row["url_private"],
        row["permalink"],
        row["download_status"],
        row["download_error"],
        row["content_sha256"],
        row["content_bytes"],
        canonical_json(row["raw_payload"]),
        row["source_run_id"],
    )


async def _flush_message_batch(pool, rows: list[dict[str, Any]]) -> int:
    if not rows:
        return 0
    attachment_rows = [
        attachment for row in rows for attachment in _attachment_rows(row)
    ]
    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.executemany(
                _MESSAGE_UPSERT_SQL, [_message_params(row) for row in rows]
            )
            for start in range(0, len(attachment_rows), ATTACHMENT_BATCH_SIZE):
                batch = attachment_rows[start : start + ATTACHMENT_BATCH_SIZE]
                await conn.executemany(
                    _ATTACHMENT_UPSERT_SQL,
                    [_attachment_params(row) for row in batch],
                )
    return len(rows)


async def import_archive_path(
    pool,
    archive_path: Path,
    *,
    import_id: str,
    workflow_run_id: str,
) -> dict[str, int]:
    run_id = f"slack_archive_{import_id}"
    counts = {
        "channels_imported": 0,
        "users_imported": 0,
        "messages_imported": 0,
        "messages_skipped": 0,
        "message_files_skipped": 0,
    }

    with zipfile.ZipFile(archive_path) as zip_file:
        channels = _json_member(zip_file, "channels.json")
        users = _json_member(zip_file, "users.json")
        if not isinstance(channels, list):
            raise RuntimeError("Slack archive channels.json must be a list")
        if not isinstance(users, list):
            raise RuntimeError("Slack archive users.json must be a list")

        channel_by_name = {
            _as_text(channel.get("name")): channel
            for channel in channels
            if isinstance(channel, dict) and channel.get("id") and channel.get("name")
        }
        channel_id_by_name = {
            name: _as_text(channel.get("id"))
            for name, channel in channel_by_name.items()
        }
        requested = _archive_channel_refs([c for c in channels if isinstance(c, dict)])
        await record_run_start(
            pool,
            run_id=run_id,
            workflow_run_id=workflow_run_id,
            mode=ARCHIVE_IMPORT_MODE,
            requested=requested,
            skipped=[],
            metadata={
                "source": "slack_archive_import",
                "import_id": import_id,
                "archive_path": str(archive_path),
            },
        )

        counts["channels_imported"] = await _upsert_archive_channels(
            pool,
            [c for c in channels if isinstance(c, dict)],
        )
        counts["users_imported"] = await _upsert_archive_users(
            pool,
            [u for u in users if isinstance(u, dict)],
        )

        message_batch: list[dict[str, Any]] = []
        for archive_file in _message_files(zip_file):
            channel_name = archive_file.split("/", 1)[0]
            channel_id = channel_id_by_name.get(channel_name)
            if not channel_id:
                counts["message_files_skipped"] += 1
                continue
            messages = _json_member(zip_file, archive_file)
            if not isinstance(messages, list):
                counts["message_files_skipped"] += 1
                continue
            for message in messages:
                if not isinstance(message, dict):
                    counts["messages_skipped"] += 1
                    continue
                row = _archive_message_row(
                    channel_id,
                    message,
                    run_id=run_id,
                    archive_file=archive_file,
                )
                if row is None:
                    counts["messages_skipped"] += 1
                    continue
                message_batch.append(row)
                if len(message_batch) >= MESSAGE_BATCH_SIZE:
                    counts["messages_imported"] += await _flush_message_batch(
                        pool,
                        message_batch,
                    )
                    message_batch = []
        counts["messages_imported"] += await _flush_message_batch(pool, message_batch)

    await record_run_finish(
        pool,
        run_id=run_id,
        status="completed",
        synced=requested,
        skipped=[],
        failed=[],
        counts={
            "messages_fetched": counts["messages_imported"]
            + counts["messages_skipped"],
            "messages_upserted": counts["messages_imported"],
        },
    )
    return counts


def _s3_client():
    import boto3
    from botocore.config import Config

    region = os.getenv("SLACK_ARCHIVE_UPLOAD_REGION") or "auto"
    endpoint_url = os.getenv("SLACK_ARCHIVE_UPLOAD_ENDPOINT") or None
    return boto3.client(
        "s3",
        region_name=region,
        endpoint_url=endpoint_url,
        config=Config(s3={"addressing_style": "path"}),
    )


def _download_s3_object(bucket: str, key: str, destination: Path) -> None:
    with destination.open("wb") as handle:
        _s3_client().download_fileobj(bucket, key, handle)


async def _download_archive(import_row: dict[str, Any], destination: Path) -> None:
    await asyncio.to_thread(
        _download_s3_object,
        _as_text(import_row.get("object_bucket")),
        _as_text(import_row.get("object_key")),
        destination,
    )


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    if not inp.import_id:
        raise RuntimeError("import_id is required")
    import_row = await _load_archive_import(ctx._pool, inp.import_id)
    if import_row.get("status") not in {"uploaded", "importing"}:
        raise RuntimeError(
            f"archive import {inp.import_id} must be uploaded before ingestion; "
            f"got {import_row.get('status')}"
        )

    await _mark_import_running(ctx._pool, import_id=inp.import_id, run_id=ctx.run_id)
    with tempfile.TemporaryDirectory(prefix="slack-archive-import-") as temp_dir:
        archive_path = Path(temp_dir) / "archive.zip"
        try:
            await _download_archive(import_row, archive_path)
            counts = await import_archive_path(
                ctx._pool,
                archive_path,
                import_id=inp.import_id,
                workflow_run_id=ctx.run_id,
            )
            await _mark_import_completed(
                ctx._pool,
                import_id=inp.import_id,
                counts=counts,
            )
        except Exception as exc:
            await _mark_import_failed(
                ctx._pool, import_id=inp.import_id, error_text=str(exc)
            )
            raise

    return {"status": "completed", "import_id": inp.import_id, "counts": counts}
