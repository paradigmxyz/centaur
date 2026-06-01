"""Workflow: project synced source rows into company context documents."""

from __future__ import annotations

import datetime as dt
import hashlib
import os
import re
from dataclasses import dataclass, field
from typing import Any

from api.runtime_control import canonical_json, decode_jsonb
from api.vm_metrics import (
    observe_company_context_document_size,
    record_company_context_documents_changed,
)
from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "company_context_documents"

DEFAULT_SYNC_INTERVAL_SECONDS = 4 * 60 * 60
DEFAULT_WATERMARK_OVERLAP_SECONDS = 60
MIN_THREAD_MESSAGES = 5
FALSE_ENV_VALUES = {"0", "false", "no", "off"}
SLACK_MENTION_RE = re.compile(r"<@([A-Z0-9]+)>")
SLACK_CHANNEL_RE = re.compile(r"<#([A-Z0-9]+)(?:\|([^>]+))?>")


def _positive_int(value: int | str | None, default: int) -> int:
    """Coerce positive integer config values with a safe default."""
    try:
        parsed = int(value) if value is not None else default
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default


def _nonnegative_int(value: int | str | None, default: int) -> int:
    """Coerce nonnegative integer config values with a safe default."""
    try:
        parsed = int(value) if value is not None else default
    except (TypeError, ValueError):
        return default
    return parsed if parsed >= 0 else default


def _env_flag_enabled(name: str, default: bool = False) -> bool:
    """Read a boolean feature flag where common false strings opt out."""
    value = os.getenv(name)
    if value is None:
        return default
    return value.strip().lower() not in FALSE_ENV_VALUES


SCHEDULE = {
    "schedule_id": "company_context_documents",
    "interval_seconds": _positive_int(
        os.getenv("COMPANY_CONTEXT_DOCUMENTS_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": (
        (
            _env_flag_enabled("SLACK_ETL_ENABLED")
            or _env_flag_enabled("GOOGLE_DRIVE_ETL_ENABLED")
            or _env_flag_enabled("GOOGLE_CALENDAR_ETL_ENABLED")
        )
        and _env_flag_enabled("COMPANY_CONTEXT_DOCUMENTS_ENABLED", default=True)
    ),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for projecting synced source rows into context documents."""

    since: str | None = None
    watermark_overlap_seconds: int = DEFAULT_WATERMARK_OVERLAP_SECONDS
    metadata: dict[str, Any] = field(default_factory=dict)


def _parse_datetime(value: str | None) -> dt.datetime | None:
    """Parse an ISO timestamp into an aware UTC datetime."""
    if not value:
        return None
    with_value = value.replace("Z", "+00:00")
    try:
        parsed = dt.datetime.fromisoformat(with_value)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)


def _format_time(value: dt.datetime | None) -> str:
    """Format document timestamps consistently for context text."""
    if not value:
        return "unknown time"
    return value.astimezone(dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")


def _display_name(row: Any) -> str:
    """Return the most useful Slack user display name from a joined row."""
    for key in ("real_name", "display_name", "user_name", "user_id"):
        value = row.get(key) if hasattr(row, "get") else row[key]
        if value:
            return str(value)
    return "Unknown"


def _sanitize_heading(text: str, limit: int = 80) -> str:
    """Collapse message text into a compact heading."""
    cleaned = re.sub(r"\s+", " ", text).strip()
    if not cleaned:
        return "Slack thread"
    return cleaned[:limit]


def _resolve_slack_mentions(
    text: str,
    *,
    users_by_id: dict[str, str],
    channels_by_id: dict[str, str],
) -> str:
    """Resolve common Slack user/channel mention tokens into readable names."""

    def user_repl(match: re.Match[str]) -> str:
        user_id = match.group(1)
        return f"@{users_by_id.get(user_id, user_id)}"

    def channel_repl(match: re.Match[str]) -> str:
        channel_id = match.group(1)
        label = match.group(2) or channels_by_id.get(channel_id) or channel_id
        return f"#{label}"

    resolved = SLACK_MENTION_RE.sub(user_repl, text)
    return SLACK_CHANNEL_RE.sub(channel_repl, resolved)


def _content_hash(*parts: Any) -> str:
    """Hash projected document content so future syncs can detect changes cheaply."""
    return hashlib.sha256(canonical_json(parts).encode("utf-8")).hexdigest()


def _source_enabled() -> bool:
    return (
        _env_flag_enabled("SLACK_ETL_ENABLED")
        or _env_flag_enabled("GOOGLE_DRIVE_ETL_ENABLED")
        or _env_flag_enabled("GOOGLE_CALENDAR_ETL_ENABLED")
    )


async def _latest_successful_watermark(pool, current_run_id: str) -> dt.datetime | None:
    """Load the last successful projection watermark from workflow output."""
    row = await pool.fetchrow(
        "SELECT output_json FROM workflow_runs "
        "WHERE workflow_name = $1 "
        "  AND run_id <> $2 "
        "  AND status = 'completed' "
        "  AND output_json IS NOT NULL "
        "ORDER BY completed_at DESC NULLS LAST, updated_at DESC "
        "LIMIT 1",
        WORKFLOW_NAME,
        current_run_id,
    )
    if not row:
        return None
    output = decode_jsonb(row["output_json"], {})
    return _parse_datetime(str(output.get("watermark") or ""))


async def _load_slack_lookup_maps(pool) -> tuple[dict[str, str], dict[str, str]]:
    """Load Slack user/channel name maps for document rendering."""
    user_rows = await pool.fetch(
        "SELECT user_id, user_name, real_name, display_name FROM slack_sync_users",
    )
    channel_rows = await pool.fetch(
        "SELECT channel_id, channel_name FROM slack_sync_channels",
    )
    users_by_id = {str(row["user_id"]): _display_name(row) for row in user_rows}
    channels_by_id = {
        str(row["channel_id"]): str(row["channel_name"] or row["channel_id"])
        for row in channel_rows
    }
    return users_by_id, channels_by_id


async def _load_changed_message_keys(pool, since: dt.datetime | None) -> dict[str, Any]:
    """Find channel/day and thread aggregates affected by changed Slack rows."""
    if since is None:
        where_sql = ""
        args: list[Any] = []
    else:
        where_sql = "WHERE updated_at > $1"
        args = [since]

    channel_day_rows = await pool.fetch(
        "SELECT DISTINCT channel_id, (occurred_at AT TIME ZONE 'UTC')::date AS day "
        f"FROM slack_sync_messages {where_sql} "
        f"{'AND' if where_sql else 'WHERE'} occurred_at IS NOT NULL "
        "ORDER BY channel_id, day",
        *args,
    )
    thread_rows = await pool.fetch(
        f"SELECT DISTINCT channel_id, thread_ts FROM slack_sync_messages {where_sql} "
        f"{'AND' if where_sql else 'WHERE'} thread_ts IS NOT NULL AND thread_ts <> '' "
        "ORDER BY channel_id, thread_ts",
        *args,
    )
    stats = await pool.fetchrow(
        f"SELECT COUNT(*) AS changed_messages, MAX(updated_at) AS max_updated_at "
        f"FROM slack_sync_messages {where_sql}",
        *args,
    )

    max_updated_at = stats["max_updated_at"] if stats else None
    if isinstance(max_updated_at, dt.datetime):
        max_updated_at = max_updated_at.astimezone(dt.timezone.utc)

    return {
        "channel_days": [
            (str(row["channel_id"]), row["day"])
            for row in channel_day_rows
            if isinstance(row["day"], dt.date)
        ],
        "threads": [
            (str(row["channel_id"]), str(row["thread_ts"])) for row in thread_rows
        ],
        "changed_messages": int(stats["changed_messages"] or 0) if stats else 0,
        "max_updated_at": max_updated_at,
    }


async def _load_changed_drive_files(pool, since: dt.datetime | None) -> dict[str, Any]:
    """Find Google Drive files whose synced content changed."""
    if since is None:
        where_sql = "WHERE last_error = '' AND trashed = FALSE"
        args: list[Any] = []
    else:
        where_sql = "WHERE last_error = '' AND trashed = FALSE AND updated_at > $1"
        args = [since]

    rows = await pool.fetch(
        "SELECT file_id, name, mime_type, web_view_link, drive_id, parent_ids, owners, "
        "last_modifying_user, source_created_at, source_modified_at, text_content, "
        "text_hash, raw_payload, updated_at "
        f"FROM google_drive_sync_files {where_sql} "
        "ORDER BY source_modified_at NULLS LAST, file_id",
        *args,
    )
    stats = await pool.fetchrow(
        f"SELECT COUNT(*) AS changed_files, MAX(updated_at) AS max_updated_at "
        f"FROM google_drive_sync_files {where_sql}",
        *args,
    )
    max_updated_at = stats["max_updated_at"] if stats else None
    if isinstance(max_updated_at, dt.datetime):
        max_updated_at = max_updated_at.astimezone(dt.timezone.utc)
    return {
        "files": list(rows),
        "changed_files": int(stats["changed_files"] or 0) if stats else 0,
        "max_updated_at": max_updated_at,
    }


async def _load_changed_calendar_events(
    pool,
    since: dt.datetime | None,
) -> dict[str, Any]:
    """Find Google Calendar events whose synced content changed."""
    if since is None:
        where_sql = "WHERE e.last_error = ''"
        args: list[Any] = []
    else:
        where_sql = "WHERE e.last_error = '' AND e.updated_at > $1"
        args = [since]

    rows = await pool.fetch(
        "SELECT e.calendar_id, c.summary AS calendar_summary, c.time_zone, "
        "e.event_id, e.i_cal_uid, e.status, e.summary, e.description, e.location, "
        "e.html_link, e.creator, e.organizer, e.attendees, e.start_payload, "
        "e.end_payload, e.start_at, e.end_at, e.is_all_day, e.recurring_event_id, "
        "e.original_start, e.transparency, e.visibility, e.event_type, e.sequence, "
        "e.source_created_at, e.source_updated_at, e.content_text, e.content_hash, "
        "e.raw_payload, e.updated_at "
        "FROM google_calendar_sync_events e "
        "LEFT JOIN google_calendar_sync_calendars c ON c.calendar_id = e.calendar_id "
        f"{where_sql} "
        "ORDER BY e.source_updated_at NULLS LAST, e.start_at NULLS LAST, "
        "e.calendar_id, e.event_id",
        *args,
    )
    stats = await pool.fetchrow(
        f"SELECT COUNT(*) AS changed_events, MAX(updated_at) AS max_updated_at "
        f"FROM google_calendar_sync_events e {where_sql}",
        *args,
    )
    max_updated_at = stats["max_updated_at"] if stats else None
    if isinstance(max_updated_at, dt.datetime):
        max_updated_at = max_updated_at.astimezone(dt.timezone.utc)
    return {
        "events": list(rows),
        "changed_events": int(stats["changed_events"] or 0) if stats else 0,
        "max_updated_at": max_updated_at,
    }


async def _load_channel_day_messages(pool, channel_id: str, day: dt.date) -> list[Any]:
    """Load all messages for one Slack channel/day aggregate."""
    start = dt.datetime.combine(day, dt.time.min, tzinfo=dt.timezone.utc)
    end = start + dt.timedelta(days=1)
    return list(
        await pool.fetch(
            "SELECT m.channel_id, c.channel_name, m.message_ts, m.occurred_at, "
            "m.thread_ts, m.parent_message_ts, m.user_id, u.user_name, u.real_name, "
            "u.display_name, m.text, m.permalink, m.reply_count, m.updated_at "
            "FROM slack_sync_messages m "
            "LEFT JOIN slack_sync_channels c ON c.channel_id = m.channel_id "
            "LEFT JOIN slack_sync_users u ON u.user_id = m.user_id "
            "WHERE m.channel_id = $1 "
            "  AND m.occurred_at >= $2 "
            "  AND m.occurred_at < $3 "
            "ORDER BY m.occurred_at, m.message_ts",
            channel_id,
            start,
            end,
        )
    )


async def _load_thread_messages(pool, channel_id: str, thread_ts: str) -> list[Any]:
    """Load all messages for one Slack thread aggregate."""
    return list(
        await pool.fetch(
            "SELECT m.channel_id, c.channel_name, m.message_ts, m.occurred_at, "
            "m.thread_ts, m.parent_message_ts, m.user_id, u.user_name, u.real_name, "
            "u.display_name, m.text, m.permalink, m.reply_count, m.updated_at "
            "FROM slack_sync_messages m "
            "LEFT JOIN slack_sync_channels c ON c.channel_id = m.channel_id "
            "LEFT JOIN slack_sync_users u ON u.user_id = m.user_id "
            "WHERE m.channel_id = $1 "
            "  AND m.thread_ts = $2 "
            "ORDER BY m.occurred_at, m.message_ts",
            channel_id,
            thread_ts,
        )
    )


def _channel_day_document(
    *,
    channel_id: str,
    day: dt.date,
    messages: list[Any],
    users_by_id: dict[str, str],
    channels_by_id: dict[str, str],
) -> dict[str, Any] | None:
    """Render one channel/day transcript document from Slack message rows."""
    if not messages:
        return None

    channel_name = str(
        messages[0]["channel_name"] or channels_by_id.get(channel_id) or channel_id
    )
    title = f"#{channel_name} - {day.isoformat()}"
    lines = [f"# {title}", ""]
    last_updated = max(
        row["updated_at"].astimezone(dt.timezone.utc) for row in messages
    )
    occurred_at = messages[0]["occurred_at"]

    for row in messages:
        speaker = _display_name(row)
        text = _resolve_slack_mentions(
            str(row["text"] or ""),
            users_by_id=users_by_id,
            channels_by_id=channels_by_id,
        )
        reply_count = int(row["reply_count"] or 0)
        reply_suffix = f" - {reply_count} replies" if reply_count else ""
        lines.extend(
            [
                f"### {speaker} - {_format_time(row['occurred_at'])}{reply_suffix}",
                "",
                text,
                "",
            ]
        )

    body = "\n".join(lines).strip()
    source_document_id = f"{channel_id}:{day.isoformat()}"
    metadata = {
        "channel_id": channel_id,
        "channel_name": channel_name,
        "date": day.isoformat(),
        "message_count": len(messages),
        "aggregation": "channel_day",
    }
    return {
        "document_id": f"slack:channel_day:{channel_id}:{day.isoformat()}",
        "source": "slack",
        "source_type": "slack_channel_day",
        "source_document_id": source_document_id,
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": title,
        "body": body,
        "url": "",
        "author_id": "",
        "author_name": "",
        "access_scope": "company",
        "occurred_at": occurred_at,
        "source_updated_at": last_updated,
        "content_hash": _content_hash(title, body, "", metadata),
        "metadata": metadata,
    }


def _thread_document(
    *,
    channel_id: str,
    thread_ts: str,
    messages: list[Any],
    users_by_id: dict[str, str],
    channels_by_id: dict[str, str],
) -> dict[str, Any] | None:
    """Render one Slack thread document using Metronome's 5+ message threshold."""
    if len(messages) < MIN_THREAD_MESSAGES:
        return None

    channel_name = str(
        messages[0]["channel_name"] or channels_by_id.get(channel_id) or channel_id
    )
    first = messages[0]
    first_text = _resolve_slack_mentions(
        str(first["text"] or ""),
        users_by_id=users_by_id,
        channels_by_id=channels_by_id,
    )
    title = _sanitize_heading(first_text)
    participants = sorted({_display_name(row) for row in messages if row["user_id"]})
    last_updated = max(
        row["updated_at"].astimezone(dt.timezone.utc) for row in messages
    )
    permalink = str(first["permalink"] or "")
    source_document_id = f"{channel_id}:{thread_ts}"

    lines = [
        f"# {title}",
        "",
        f"- Channel: #{channel_name}",
        f"- Started: {_format_time(first['occurred_at'])}",
        f"- Participants: {', '.join(participants)}",
        f"- Replies: {len(messages) - 1}",
        f"- URL: {permalink}",
        "",
        "---",
        "",
    ]
    for row in messages:
        speaker = _display_name(row)
        text = _resolve_slack_mentions(
            str(row["text"] or ""),
            users_by_id=users_by_id,
            channels_by_id=channels_by_id,
        )
        lines.extend(
            [f"### {speaker} - {_format_time(row['occurred_at'])}", "", text, ""]
        )

    body = "\n".join(lines).strip()
    metadata = {
        "channel_id": channel_id,
        "channel_name": channel_name,
        "thread_ts": thread_ts,
        "message_count": len(messages),
        "reply_count": len(messages) - 1,
        "participants": participants,
        "aggregation": "thread",
    }
    return {
        "document_id": f"slack:thread:{channel_id}:{thread_ts}",
        "source": "slack",
        "source_type": "slack_thread",
        "source_document_id": source_document_id,
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": title,
        "body": body,
        "url": permalink,
        "author_id": str(first["user_id"] or ""),
        "author_name": _display_name(first),
        "access_scope": "company",
        "occurred_at": first["occurred_at"],
        "source_updated_at": last_updated,
        "content_hash": _content_hash(title, body, permalink, metadata),
        "metadata": metadata,
    }


def _jsonb_value(row: Any, key: str, default: Any) -> Any:
    value = row.get(key) if hasattr(row, "get") else row[key]
    return decode_jsonb(value, default)


def _drive_author(row: Any) -> tuple[str, str]:
    owners = _jsonb_value(row, "owners", [])
    if isinstance(owners, list):
        for owner in owners:
            if not isinstance(owner, dict):
                continue
            email = str(owner.get("emailAddress") or "").strip()
            name = str(owner.get("displayName") or email).strip()
            if email or name:
                return email, name
    last_modifying_user = _jsonb_value(row, "last_modifying_user", {})
    if isinstance(last_modifying_user, dict):
        email = str(last_modifying_user.get("emailAddress") or "").strip()
        name = str(last_modifying_user.get("displayName") or email).strip()
        if email or name:
            return email, name
    return "", ""


def _drive_document(row: Any) -> dict[str, Any] | None:
    """Render one synced Google Doc into a context document."""
    file_id = str(row["file_id"] or "")
    if not file_id:
        return None
    title = str(row["name"] or "Untitled Google Doc")
    text = str(row["text_content"] or "").strip()
    url = str(row["web_view_link"] or "")
    parent_ids = _jsonb_value(row, "parent_ids", [])
    owners = _jsonb_value(row, "owners", [])
    last_modifying_user = _jsonb_value(row, "last_modifying_user", {})
    raw_payload = _jsonb_value(row, "raw_payload", {})
    author_id, author_name = _drive_author(row)
    source_modified_at = row["source_modified_at"]
    source_created_at = row["source_created_at"]

    lines = [
        f"# {title}",
        "",
        "- Source: Google Drive",
        f"- Modified: {_format_time(source_modified_at)}",
    ]
    if url:
        lines.append(f"- URL: {url}")
    lines.extend(["", "---", "", text])
    body = "\n".join(lines).strip()
    metadata = {
        "file_id": file_id,
        "drive_id": str(row["drive_id"] or ""),
        "parent_ids": parent_ids if isinstance(parent_ids, list) else [],
        "owners": owners if isinstance(owners, list) else [],
        "last_modifying_user": (
            last_modifying_user if isinstance(last_modifying_user, dict) else {}
        ),
        "mime_type": str(row["mime_type"] or ""),
        "raw_payload": raw_payload if isinstance(raw_payload, dict) else {},
    }
    return {
        "document_id": f"google_drive:doc:{file_id}",
        "source": "google_drive",
        "source_type": "google_doc",
        "source_document_id": file_id,
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": title,
        "body": body,
        "url": url,
        "author_id": author_id,
        "author_name": author_name,
        "access_scope": "company",
        "occurred_at": source_created_at or source_modified_at,
        "source_updated_at": source_modified_at,
        "content_hash": _content_hash(title, body, url, metadata),
        "metadata": metadata,
    }


def _calendar_person(value: Any) -> tuple[str, str]:
    if not isinstance(value, dict):
        return "", ""
    email = str(value.get("email") or "").strip()
    name = str(value.get("displayName") or email).strip()
    return email, name


def _calendar_attendee_names(attendees: Any) -> list[str]:
    if not isinstance(attendees, list):
        return []
    names: list[str] = []
    for attendee in attendees:
        if not isinstance(attendee, dict):
            continue
        email = str(attendee.get("email") or "").strip()
        name = str(attendee.get("displayName") or email).strip()
        if name:
            names.append(name)
    return names


def _calendar_event_document(row: Any) -> dict[str, Any] | None:
    """Render one synced Google Calendar event into a context document."""
    calendar_id = str(row["calendar_id"] or "")
    event_id = str(row["event_id"] or "")
    if not calendar_id or not event_id:
        return None
    title = str(row["summary"] or "Untitled calendar event")
    description = str(row["description"] or "").strip()
    location = str(row["location"] or "").strip()
    url = str(row["html_link"] or "")
    calendar_summary = str(row["calendar_summary"] or calendar_id)
    status = str(row["status"] or "")
    creator = _jsonb_value(row, "creator", {})
    organizer = _jsonb_value(row, "organizer", {})
    attendees = _jsonb_value(row, "attendees", [])
    attendee_names = _calendar_attendee_names(attendees)
    author_id, author_name = _calendar_person(organizer)
    if not author_id and not author_name:
        author_id, author_name = _calendar_person(creator)

    start_at = row["start_at"]
    end_at = row["end_at"]
    source_updated_at = row["source_updated_at"]
    source_created_at = row["source_created_at"]
    occurred_at = start_at or source_created_at or source_updated_at
    lines = [
        f"# {title}",
        "",
        "- Source: Google Calendar",
        f"- Calendar: {calendar_summary}",
        f"- Status: {status or 'unknown'}",
        f"- Starts: {_format_time(start_at)}",
        f"- Ends: {_format_time(end_at)}",
    ]
    if location:
        lines.append(f"- Location: {location}")
    if attendee_names:
        lines.append(f"- Attendees: {', '.join(attendee_names)}")
    if url:
        lines.append(f"- URL: {url}")
    if description:
        lines.extend(["", "---", "", description])
    body = "\n".join(lines).strip()
    metadata = {
        "calendar_id": calendar_id,
        "calendar_summary": calendar_summary,
        "event_id": event_id,
        "i_cal_uid": str(row["i_cal_uid"] or ""),
        "status": status,
        "location": location,
        "creator": creator if isinstance(creator, dict) else {},
        "organizer": organizer if isinstance(organizer, dict) else {},
        "attendees": attendees if isinstance(attendees, list) else [],
        "is_all_day": bool(row["is_all_day"]),
        "recurring_event_id": str(row["recurring_event_id"] or ""),
        "transparency": str(row["transparency"] or ""),
        "visibility": str(row["visibility"] or ""),
        "event_type": str(row["event_type"] or ""),
    }
    return {
        "document_id": f"google_calendar:event:{calendar_id}:{event_id}",
        "source": "google_calendar",
        "source_type": "calendar_event",
        "source_document_id": f"{calendar_id}:{event_id}",
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": title,
        "body": body,
        "url": url,
        "author_id": author_id,
        "author_name": author_name,
        "access_scope": "company",
        "occurred_at": occurred_at,
        "source_updated_at": source_updated_at,
        "content_hash": _content_hash(title, body, url, metadata),
        "metadata": metadata,
    }


def _calendar_event_document_id(row: Any) -> str:
    calendar_id = str(row["calendar_id"] or "")
    event_id = str(row["event_id"] or "")
    return f"google_calendar:event:{calendar_id}:{event_id}"


async def _upsert_document(pool, document: dict[str, Any]) -> str:
    """Upsert a projected document and return inserted/updated/noop."""
    existing_hash = await pool.fetchval(
        "SELECT content_hash FROM company_context_documents WHERE document_id = $1",
        document["document_id"],
    )
    if existing_hash == document["content_hash"]:
        return "noop"

    status = await pool.execute(
        "INSERT INTO company_context_documents ("
        "document_id, source, source_type, source_document_id, source_chunk_id, "
        "parent_document_id, title, body, url, author_id, author_name, access_scope, "
        "occurred_at, source_updated_at, content_hash, metadata, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, "
        "$15, $16::jsonb, NOW()"
        ") ON CONFLICT (document_id) DO UPDATE SET "
        "source = EXCLUDED.source, "
        "source_type = EXCLUDED.source_type, "
        "source_document_id = EXCLUDED.source_document_id, "
        "source_chunk_id = EXCLUDED.source_chunk_id, "
        "parent_document_id = EXCLUDED.parent_document_id, "
        "title = EXCLUDED.title, "
        "body = EXCLUDED.body, "
        "url = EXCLUDED.url, "
        "author_id = EXCLUDED.author_id, "
        "author_name = EXCLUDED.author_name, "
        "access_scope = EXCLUDED.access_scope, "
        "occurred_at = EXCLUDED.occurred_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "content_hash = EXCLUDED.content_hash, "
        "metadata = EXCLUDED.metadata, "
        "updated_at = NOW() "
        "WHERE company_context_documents.content_hash IS DISTINCT FROM EXCLUDED.content_hash",
        document["document_id"],
        document["source"],
        document["source_type"],
        document["source_document_id"],
        document["source_chunk_id"],
        document["parent_document_id"],
        document["title"],
        document["body"],
        document["url"],
        document["author_id"],
        document["author_name"],
        document["access_scope"],
        document["occurred_at"],
        document["source_updated_at"],
        document["content_hash"],
        canonical_json(document["metadata"]),
    )
    if not status.endswith(" 1"):
        return "noop"
    return "updated" if existing_hash else "inserted"


async def _delete_document(pool, document_id: str) -> bool:
    """Remove a derived document that no longer meets projection criteria."""
    status = await pool.execute(
        "DELETE FROM company_context_documents WHERE document_id = $1",
        document_id,
    )
    return status.endswith(" 1")


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Project changed sync rows into embeddable company context documents."""
    if not (
        _source_enabled()
        and _env_flag_enabled("COMPANY_CONTEXT_DOCUMENTS_ENABLED", default=True)
    ):
        ctx.log("company_context_documents_skipped_disabled")
        return {"status": "skipped", "reason": "company_context_documents_disabled"}

    explicit_since = _parse_datetime(inp.since)
    last_watermark = explicit_since or await _latest_successful_watermark(
        ctx._pool, ctx.run_id
    )
    overlap_seconds = _nonnegative_int(
        inp.watermark_overlap_seconds,
        DEFAULT_WATERMARK_OVERLAP_SECONDS,
    )
    since = (
        last_watermark - dt.timedelta(seconds=overlap_seconds)
        if last_watermark is not None
        else None
    )

    slack_enabled = _env_flag_enabled("SLACK_ETL_ENABLED")
    google_drive_enabled = _env_flag_enabled("GOOGLE_DRIVE_ETL_ENABLED")
    google_calendar_enabled = _env_flag_enabled("GOOGLE_CALENDAR_ETL_ENABLED")
    changed = {
        "channel_days": [],
        "threads": [],
        "changed_messages": 0,
        "max_updated_at": None,
    }
    users_by_id: dict[str, str] = {}
    channels_by_id: dict[str, str] = {}
    if slack_enabled:
        users_by_id, channels_by_id = await _load_slack_lookup_maps(ctx._pool)
        changed = await _load_changed_message_keys(ctx._pool, since)
    drive_changed = {
        "files": [],
        "changed_files": 0,
        "max_updated_at": None,
    }
    if google_drive_enabled:
        drive_changed = await _load_changed_drive_files(ctx._pool, since)
    calendar_changed = {
        "events": [],
        "changed_events": 0,
        "max_updated_at": None,
    }
    if google_calendar_enabled:
        calendar_changed = await _load_changed_calendar_events(ctx._pool, since)

    documents_upserted = 0
    documents_deleted = 0
    for channel_id, day in changed["channel_days"]:
        messages = await _load_channel_day_messages(ctx._pool, channel_id, day)
        document = _channel_day_document(
            channel_id=channel_id,
            day=day,
            messages=messages,
            users_by_id=users_by_id,
            channels_by_id=channels_by_id,
        )
        if document is None:
            if await _delete_document(
                ctx._pool,
                f"slack:channel_day:{channel_id}:{day.isoformat()}",
            ):
                documents_deleted += 1
                record_company_context_documents_changed(
                    "slack",
                    "slack_channel_day",
                    "deleted",
                )
            continue
        observe_company_context_document_size(
            "slack",
            str(document["source_type"]),
            len(str(document["body"] or "")),
        )
        action = await _upsert_document(ctx._pool, document)
        record_company_context_documents_changed(
            "slack",
            str(document["source_type"]),
            action,
        )
        if action in {"inserted", "updated"}:
            documents_upserted += 1

    for channel_id, thread_ts in changed["threads"]:
        messages = await _load_thread_messages(ctx._pool, channel_id, thread_ts)
        document = _thread_document(
            channel_id=channel_id,
            thread_ts=thread_ts,
            messages=messages,
            users_by_id=users_by_id,
            channels_by_id=channels_by_id,
        )
        if document is None:
            if await _delete_document(
                ctx._pool, f"slack:thread:{channel_id}:{thread_ts}"
            ):
                documents_deleted += 1
                record_company_context_documents_changed(
                    "slack",
                    "slack_thread",
                    "deleted",
                )
            continue
        observe_company_context_document_size(
            "slack",
            str(document["source_type"]),
            len(str(document["body"] or "")),
        )
        action = await _upsert_document(ctx._pool, document)
        record_company_context_documents_changed(
            "slack",
            str(document["source_type"]),
            action,
        )
        if action in {"inserted", "updated"}:
            documents_upserted += 1

    for row in drive_changed["files"]:
        document = _drive_document(row)
        if document is None:
            continue
        observe_company_context_document_size(
            "google_drive",
            str(document["source_type"]),
            len(str(document["body"] or "")),
        )
        action = await _upsert_document(ctx._pool, document)
        record_company_context_documents_changed(
            "google_drive",
            str(document["source_type"]),
            action,
        )
        if action in {"inserted", "updated"}:
            documents_upserted += 1

    for row in calendar_changed["events"]:
        if str(row["status"] or "") == "cancelled":
            if await _delete_document(ctx._pool, _calendar_event_document_id(row)):
                documents_deleted += 1
                record_company_context_documents_changed(
                    "google_calendar",
                    "calendar_event",
                    "deleted",
                )
            continue
        document = _calendar_event_document(row)
        if document is None:
            continue
        observe_company_context_document_size(
            "google_calendar",
            str(document["source_type"]),
            len(str(document["body"] or "")),
        )
        action = await _upsert_document(ctx._pool, document)
        record_company_context_documents_changed(
            "google_calendar",
            str(document["source_type"]),
            action,
        )
        if action in {"inserted", "updated"}:
            documents_upserted += 1

    watermark_candidates = [
        value
        for value in (
            changed["max_updated_at"],
            drive_changed["max_updated_at"],
            calendar_changed["max_updated_at"],
            last_watermark,
        )
        if value is not None
    ]
    watermark = max(watermark_candidates) if watermark_candidates else None
    result = {
        "status": "completed",
        "changed_messages": changed["changed_messages"],
        "changed_drive_files": drive_changed["changed_files"],
        "changed_calendar_events": calendar_changed["changed_events"],
        "channel_day_documents": len(changed["channel_days"]),
        "thread_candidates": len(changed["threads"]),
        "drive_documents": len(drive_changed["files"]),
        "calendar_event_documents": len(calendar_changed["events"]),
        "documents_upserted": documents_upserted,
        "documents_deleted": documents_deleted,
        "watermark": watermark.isoformat() if watermark else None,
    }
    ctx.log("company_context_documents_completed", **result)
    return result
