"""Workflow: sync Granola notes and transcripts into Postgres."""

from __future__ import annotations

import datetime as dt
import hashlib
import os
from dataclasses import dataclass, field
from typing import Any, Protocol

from api.runtime_control import canonical_json
from workflows.etl_metrics import (
    record_etl_items_failed,
    record_etl_items_seen,
    record_etl_items_upserted,
    set_etl_active_scopes,
    set_etl_failed_scopes,
    set_etl_scope_sync_freshness_seconds,
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import env_flag_enabled, positive_int

WORKFLOW_NAME = "granola_sync"
DEFAULT_SYNC_INTERVAL_SECONDS = 4 * 60 * 60
DEFAULT_PAGE_SIZE = 30
DEFAULT_WATERMARK_OVERLAP_SECONDS = 5 * 60
WORKSPACE_SCOPE = "workspace"


SCHEDULE = {
    "schedule_id": "granola_sync",
    "interval_seconds": positive_int(
        os.getenv("GRANOLA_SYNC_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": env_flag_enabled("GRANOLA_ETL_ENABLED", default=False),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for a manual Granola sync workflow run."""

    since: str | None = None
    limit: int = DEFAULT_PAGE_SIZE
    max_notes: int | None = None
    include_transcripts: bool = True
    watermark_overlap_seconds: int = DEFAULT_WATERMARK_OVERLAP_SECONDS
    metadata: dict[str, Any] = field(default_factory=dict)


class GranolaSyncClient(Protocol):
    """Small adapter protocol used by the Granola ETL workflow."""

    async def list_notes(
        self,
        page_size: int = DEFAULT_PAGE_SIZE,
        cursor: str | None = None,
        created_before: str | None = None,
        created_after: str | None = None,
        updated_after: str | None = None,
    ) -> dict[str, Any]: ...

    async def get_note(
        self, note_id: str, include_transcript: bool = False
    ) -> dict[str, Any]: ...


class GranolaToolClient:
    """Granola client backed by the workflow tool bridge."""

    def __init__(self, ctx: WorkflowContext) -> None:
        self._ctx = ctx

    async def list_notes(
        self,
        page_size: int = DEFAULT_PAGE_SIZE,
        cursor: str | None = None,
        created_before: str | None = None,
        created_after: str | None = None,
        updated_after: str | None = None,
    ) -> dict[str, Any]:
        result = await self._ctx.call_tool(
            "granola",
            "list_notes",
            {
                "page_size": page_size,
                "cursor": cursor,
                "created_before": created_before,
                "created_after": created_after,
                "updated_after": updated_after,
            },
        )
        return result if isinstance(result, dict) else {}

    async def get_note(
        self, note_id: str, include_transcript: bool = False
    ) -> dict[str, Any]:
        result = await self._ctx.call_tool(
            "granola",
            "get_note",
            {"note_id": note_id, "include_transcript": include_transcript},
        )
        return result if isinstance(result, dict) else {}


def _client(ctx: WorkflowContext) -> GranolaSyncClient:
    return GranolaToolClient(ctx)


def _parse_datetime(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)


def _source_datetime(payload: dict[str, Any], *keys: str) -> dt.datetime | None:
    for key in keys:
        parsed = _parse_datetime(str(payload.get(key) or ""))
        if parsed is not None:
            return parsed
    return None


def _rfc3339(value: dt.datetime) -> str:
    return value.astimezone(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def _text_value(value: Any) -> str:
    return str(value or "")


def _note_url(note: dict[str, Any]) -> str:
    return _text_value(note.get("url") or note.get("permalink") or note.get("web_url"))


def _normalized_email(value: Any) -> str:
    return str(value or "").strip().lower()


def _access_emails(owner: dict[str, Any], attendees: list[Any]) -> list[str]:
    emails: list[str] = []

    def add(value: Any) -> None:
        email = _normalized_email(value)
        if email and email not in emails:
            emails.append(email)

    add(owner.get("email"))
    for attendee in attendees:
        if isinstance(attendee, dict):
            add(attendee.get("email"))
    return emails


def _json_object(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _json_array(value: Any) -> list[Any]:
    return value if isinstance(value, list) else []


def _format_time(value: dt.datetime | None) -> str:
    if not value:
        return "unknown time"
    return value.astimezone(dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")


def _named_entry(value: Any) -> str:
    if isinstance(value, dict):
        name = _text_value(value.get("name") or value.get("display_name")).strip()
        email = _text_value(value.get("email")).strip()
        if name and email:
            return f"{name} <{email}>"
        return name or email
    return _text_value(value).strip()


def _named_entries(value: Any) -> list[str]:
    labels: list[str] = []
    for entry in _json_array(value):
        label = _named_entry(entry)
        if label and label not in labels:
            labels.append(label)
    return labels


def _content_hash(*parts: Any) -> str:
    return hashlib.sha256(canonical_json(parts).encode("utf-8")).hexdigest()


def _workflow_run_id_to_sync_run_id(workflow_run_id: str) -> str:
    safe_run_id = "".join(char if char.isalnum() else "_" for char in workflow_run_id)
    return f"granola_sync_{safe_run_id}"


def _scope_ref(scope_id: str, reason: str | None = None) -> dict[str, str]:
    result = {"scope_id": scope_id}
    if reason:
        result["reason"] = reason
    return result


def _failure_reason(error: str) -> str:
    lowered = error.lower()
    if "rate" in lowered or "429" in lowered:
        return "rate_limited"
    if (
        "401" in lowered
        or "403" in lowered
        or "auth" in lowered
        or "permission" in lowered
    ):
        return "permission_error"
    if "database" in lowered or "postgres" in lowered:
        return "write_error"
    return "api_error"


def _transcript_text(transcript: Any) -> str:
    lines: list[str] = []
    for utterance in _json_array(transcript):
        if not isinstance(utterance, dict):
            continue
        speaker = _json_object(utterance.get("speaker"))
        speaker_name = (
            _text_value(speaker.get("name"))
            or _text_value(speaker.get("email"))
            or _text_value(speaker.get("source"))
            or "Unknown"
        )
        text = _text_value(utterance.get("text")).strip()
        if text:
            lines.append(f"{speaker_name}: {text}")
    return "\n".join(lines)


def _granola_context_document(
    *,
    note: dict[str, Any],
    note_id: str,
    title: str,
    owner: dict[str, Any],
    attendees: list[Any],
    access_emails: list[str],
    calendar_event: dict[str, Any],
    transcript: list[Any],
    transcript_text: str,
    summary_markdown: str,
    summary_text: str,
    source_created_at: dt.datetime | None,
    source_updated_at: dt.datetime | None,
) -> dict[str, Any]:
    owner_id = _text_value(owner.get("id") or owner.get("user_id"))
    owner_email = _text_value(owner.get("email"))
    owner_name = _text_value(owner.get("name") or owner.get("display_name"))
    owner_label = _named_entry(owner)
    attendee_labels = _named_entries(attendees)
    document_title = title.strip() or "Untitled Granola note"
    url = _note_url(note)
    summary = summary_markdown.strip() or summary_text.strip()

    lines = [
        f"# {document_title}",
        "",
        "- Source: Granola",
        f"- Created: {_format_time(source_created_at)}",
        f"- Updated: {_format_time(source_updated_at)}",
    ]
    if owner_label:
        lines.append(f"- Owner: {owner_label}")
    if attendee_labels:
        lines.append(f"- Attendees: {', '.join(attendee_labels)}")
    if url:
        lines.append(f"- URL: {url}")
    if summary:
        lines.extend(["", "## Summary", summary])
    if transcript_text.strip():
        lines.extend(["", "## Transcript", transcript_text.strip()])

    body = "\n".join(lines).strip()
    metadata = {
        "source": "granola",
        "note_id": note_id,
        "owner_id": owner_id,
        "owner_email": owner_email,
        "owner_name": owner_name,
        "access_emails": access_emails,
        "attendees": attendees,
        "attendee_labels": attendee_labels,
        "calendar_event": calendar_event,
        "transcript_payload": transcript,
        "has_summary": bool(summary),
        "has_transcript": bool(transcript_text.strip()),
        "raw_payload": note,
    }
    return {
        "document_id": f"granola:note:{note_id}",
        "note_id": note_id,
        "title": document_title,
        "body": body,
        "url": url,
        "owner_id": owner_id,
        "owner_email": owner_email,
        "owner_name": owner_name,
        "access_emails": access_emails,
        "attendee_labels": attendee_labels,
        "occurred_at": source_created_at or source_updated_at,
        "source_updated_at": source_updated_at,
        "content_hash": _content_hash(document_title, body, url, metadata),
        "metadata": metadata,
    }


async def _load_checkpoint(pool, scope_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT watermark_time, last_error FROM granola_sync_checkpoints "
        "WHERE scope_id = $1",
        scope_id,
    )
    return dict(row) if row else None


async def _update_checkpoint_success(
    pool,
    *,
    scope_id: str,
    watermark_time: dt.datetime | None,
    run_id: str,
) -> None:
    await pool.execute(
        "INSERT INTO granola_sync_checkpoints ("
        "scope_id, watermark_time, last_run_id, last_success_at, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW(), '', NOW()) "
        "ON CONFLICT (scope_id) DO UPDATE SET "
        "watermark_time = COALESCE(EXCLUDED.watermark_time, "
        "granola_sync_checkpoints.watermark_time), "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_success_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        scope_id,
        watermark_time,
        run_id,
    )


async def _update_checkpoint_failure(
    pool,
    *,
    scope_id: str,
    run_id: str,
    error: str,
) -> None:
    await pool.execute(
        "INSERT INTO granola_sync_checkpoints ("
        "scope_id, last_run_id, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW()) "
        "ON CONFLICT (scope_id) DO UPDATE SET "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_error = EXCLUDED.last_error, "
        "updated_at = NOW()",
        scope_id,
        run_id,
        error,
    )


async def _emit_checkpoint_metrics(pool) -> None:
    """Publish Granola workspace checkpoint health for the ETL overview."""
    row = await pool.fetchrow(
        "SELECT COUNT(*) AS active_scopes, "
        "COUNT(*) FILTER (WHERE last_error <> '') AS failed_scopes, "
        "COALESCE("
        "  EXTRACT(EPOCH FROM NOW() - MIN(last_success_at) "
        "    FILTER (WHERE last_success_at IS NOT NULL)"
        "  ), "
        "  0"
        ") AS freshness_seconds "
        "FROM granola_sync_checkpoints"
    )
    set_etl_active_scopes("granola", int(row["active_scopes"] or 0) if row else 0)
    set_etl_failed_scopes("granola", int(row["failed_scopes"] or 0) if row else 0)
    set_etl_scope_sync_freshness_seconds(
        "granola",
        float(row["freshness_seconds"] or 0.0) if row else 0.0,
    )


async def _record_run_start(
    pool,
    *,
    run_id: str,
    workflow_run_id: str,
    scopes_requested: list[dict[str, str]],
    metadata: dict[str, Any],
) -> None:
    await pool.execute(
        "INSERT INTO granola_sync_runs ("
        "run_id, workflow_run_id, mode, status, scopes_requested, metadata"
        ") VALUES ($1, $2, 'incremental', 'running', $3::jsonb, $4::jsonb) "
        "ON CONFLICT (run_id) DO UPDATE SET "
        "workflow_run_id = EXCLUDED.workflow_run_id, "
        "status = 'running', "
        "scopes_requested = EXCLUDED.scopes_requested, "
        "scopes_synced = '[]'::jsonb, "
        "scopes_failed = '[]'::jsonb, "
        "notes_seen = 0, "
        "notes_upserted = 0, "
        "transcripts_seen = 0, "
        "transcripts_upserted = 0, "
        "finished_at = NULL, "
        "error_text = '', "
        "metadata = EXCLUDED.metadata",
        run_id,
        workflow_run_id,
        canonical_json(scopes_requested),
        canonical_json(metadata),
    )


async def _record_run_finish(
    pool,
    *,
    run_id: str,
    status: str,
    scopes_synced: list[dict[str, str]],
    scopes_failed: list[dict[str, str]],
    counts: dict[str, int],
    error_text: str = "",
) -> None:
    await pool.execute(
        "UPDATE granola_sync_runs SET "
        "status = $2, scopes_synced = $3::jsonb, scopes_failed = $4::jsonb, "
        "notes_seen = $5, notes_upserted = $6, transcripts_seen = $7, "
        "transcripts_upserted = $8, finished_at = NOW(), error_text = $9 "
        "WHERE run_id = $1",
        run_id,
        status,
        canonical_json(scopes_synced),
        canonical_json(scopes_failed),
        counts.get("notes_seen", 0),
        counts.get("notes_upserted", 0),
        counts.get("transcripts_seen", 0),
        counts.get("transcripts_upserted", 0),
        error_text,
    )


async def _upsert_context_document(pool, document: dict[str, Any]) -> None:
    await pool.execute(
        "INSERT INTO granola_context_documents ("
        "document_id, note_id, title, body, url, owner_id, owner_email, owner_name, "
        "access_emails, attendee_labels, occurred_at, source_updated_at, content_hash, "
        "metadata, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9::text[], $10::text[], $11, $12, $13, "
        "$14::jsonb, NOW()"
        ") ON CONFLICT (document_id) DO UPDATE SET "
        "note_id = EXCLUDED.note_id, "
        "title = EXCLUDED.title, "
        "body = EXCLUDED.body, "
        "url = EXCLUDED.url, "
        "owner_id = EXCLUDED.owner_id, "
        "owner_email = EXCLUDED.owner_email, "
        "owner_name = EXCLUDED.owner_name, "
        "access_emails = EXCLUDED.access_emails, "
        "attendee_labels = EXCLUDED.attendee_labels, "
        "occurred_at = EXCLUDED.occurred_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "content_hash = EXCLUDED.content_hash, "
        "metadata = EXCLUDED.metadata, "
        "updated_at = NOW()",
        document["document_id"],
        document["note_id"],
        document["title"],
        document["body"],
        document["url"],
        document["owner_id"],
        document["owner_email"],
        document["owner_name"],
        document["access_emails"],
        document["attendee_labels"],
        document["occurred_at"],
        document["source_updated_at"],
        document["content_hash"],
        canonical_json(document["metadata"]),
    )


async def _upsert_note(
    pool,
    *,
    note: dict[str, Any],
    run_id: str,
) -> tuple[dt.datetime | None, bool]:
    note_id = _text_value(note.get("id") or note.get("note_id"))
    owner = _json_object(note.get("owner"))
    attendees = _json_array(note.get("attendees"))
    access_emails = _access_emails(owner, attendees)
    calendar_event = _json_object(note.get("calendar_event"))
    transcript = _json_array(note.get("transcript"))
    transcript_text = _transcript_text(transcript)
    summary_markdown = _text_value(note.get("summary_markdown"))
    summary_text = _text_value(note.get("summary_text"))
    title = _text_value(note.get("title"))
    content_text = "\n".join(
        part
        for part in (title, summary_markdown, summary_text, transcript_text)
        if part.strip()
    )
    source_created_at = _source_datetime(note, "created_at", "createdAt")
    source_updated_at = (
        _source_datetime(note, "updated_at", "updatedAt") or source_created_at
    )
    context_document = _granola_context_document(
        note=note,
        note_id=note_id,
        title=title,
        owner=owner,
        attendees=attendees,
        access_emails=access_emails,
        calendar_event=calendar_event,
        transcript=transcript,
        transcript_text=transcript_text,
        summary_markdown=summary_markdown,
        summary_text=summary_text,
        source_created_at=source_created_at,
        source_updated_at=source_updated_at,
    )
    await pool.execute(
        "INSERT INTO granola_sync_notes ("
        "note_id, title, owner_id, owner_email, owner_name, attendees, access_emails, "
        "calendar_event, summary_markdown, summary_text, transcript_text, transcript_payload, "
        "url, content_text, content_hash, source_created_at, source_updated_at, raw_payload, "
        "source_run_id, last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6::jsonb, $7::text[], $8::jsonb, $9, $10, "
        "$11, $12::jsonb, $13, $14, $15, $16, $17, $18::jsonb, $19, NOW(), '', NOW()"
        ") ON CONFLICT (note_id) DO UPDATE SET "
        "title = EXCLUDED.title, "
        "owner_id = EXCLUDED.owner_id, "
        "owner_email = EXCLUDED.owner_email, "
        "owner_name = EXCLUDED.owner_name, "
        "attendees = EXCLUDED.attendees, "
        "access_emails = EXCLUDED.access_emails, "
        "calendar_event = EXCLUDED.calendar_event, "
        "summary_markdown = EXCLUDED.summary_markdown, "
        "summary_text = EXCLUDED.summary_text, "
        "transcript_text = EXCLUDED.transcript_text, "
        "transcript_payload = EXCLUDED.transcript_payload, "
        "url = EXCLUDED.url, "
        "content_text = EXCLUDED.content_text, "
        "content_hash = EXCLUDED.content_hash, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        note_id,
        title,
        _text_value(owner.get("id") or owner.get("user_id")),
        _text_value(owner.get("email")),
        _text_value(owner.get("name") or owner.get("display_name")),
        canonical_json(attendees),
        access_emails,
        canonical_json(calendar_event),
        summary_markdown,
        summary_text,
        transcript_text,
        canonical_json(transcript),
        _text_value(note.get("url") or note.get("permalink")),
        content_text,
        _content_hash(content_text),
        source_created_at,
        source_updated_at,
        canonical_json(note),
        run_id,
    )
    await _upsert_context_document(pool, context_document)
    return source_updated_at, bool(transcript)


async def _sync_workspace(
    *,
    client: GranolaSyncClient,
    pool,
    page_size: int,
    updated_after: dt.datetime | None,
    max_notes: int | None,
    include_transcripts: bool,
    run_id: str,
) -> tuple[int, int, int, int, dt.datetime | None]:
    seen = 0
    upserted = 0
    transcripts_seen = 0
    transcripts_upserted = 0
    watermark: dt.datetime | None = None
    cursor: str | None = None
    updated_after_arg = _rfc3339(updated_after) if updated_after else None

    while True:
        page = await client.list_notes(
            page_size=page_size,
            cursor=cursor,
            updated_after=updated_after_arg,
        )
        notes = [
            note
            for note in page.get("notes", []) or []
            if isinstance(note, dict) and (note.get("id") or note.get("note_id"))
        ]
        if max_notes is not None:
            notes = notes[: max(max_notes - seen, 0)]
        seen += len(notes)
        record_etl_items_seen("granola", WORKSPACE_SCOPE, "note", len(notes))

        for note_ref in notes:
            note_id = _text_value(note_ref.get("id") or note_ref.get("note_id"))
            note = (
                await client.get_note(note_id, include_transcript=include_transcripts)
                if note_id
                else note_ref
            )
            if not isinstance(note, dict):
                note = note_ref
            note.setdefault("id", note_id)
            source_updated_at, has_transcript = await _upsert_note(
                pool, note=note, run_id=run_id
            )
            upserted += 1
            record_etl_items_upserted("granola", WORKSPACE_SCOPE, "note", 1)
            if include_transcripts:
                transcripts_seen += 1
                if has_transcript:
                    transcripts_upserted += 1
                    record_etl_items_upserted(
                        "granola", WORKSPACE_SCOPE, "transcript", 1
                    )
            if source_updated_at and (
                watermark is None or source_updated_at > watermark
            ):
                watermark = source_updated_at

        if max_notes is not None and seen >= max_notes:
            break
        cursor = (
            _text_value(page.get("cursor") or page.get("next_cursor")).strip() or None
        )
        if not page.get("hasMore") or not cursor:
            break

    return seen, upserted, transcripts_seen, transcripts_upserted, watermark


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Sync changed Granola notes into raw sync tables."""
    if not env_flag_enabled("GRANOLA_ETL_ENABLED", default=False):
        ctx.log("granola_sync_skipped_disabled")
        return {"status": "skipped", "reason": "granola_etl_disabled"}

    page_size = min(positive_int(inp.limit, DEFAULT_PAGE_SIZE), DEFAULT_PAGE_SIZE)
    overlap_seconds = max(int(inp.watermark_overlap_seconds), 0)
    run_id = _workflow_run_id_to_sync_run_id(ctx.run_id)
    scopes_requested = [_scope_ref(WORKSPACE_SCOPE)]

    await _record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        scopes_requested=scopes_requested,
        metadata={
            **inp.metadata,
            "page_size": page_size,
            "max_notes": inp.max_notes,
            "include_transcripts": inp.include_transcripts,
        },
    )

    client = _client(ctx)
    explicit_since = _parse_datetime(inp.since)
    checkpoint = await _load_checkpoint(ctx._pool, WORKSPACE_SCOPE)
    watermark = explicit_since
    if watermark is None and checkpoint and checkpoint.get("watermark_time"):
        watermark = checkpoint["watermark_time"].astimezone(dt.timezone.utc)
    if watermark is not None:
        watermark = watermark - dt.timedelta(seconds=overlap_seconds)

    synced: list[dict[str, str]] = []
    failed: list[dict[str, str]] = []
    counts = {
        "notes_seen": 0,
        "notes_upserted": 0,
        "transcripts_seen": 0,
        "transcripts_upserted": 0,
    }
    try:
        (
            counts["notes_seen"],
            counts["notes_upserted"],
            counts["transcripts_seen"],
            counts["transcripts_upserted"],
            successful_watermark,
        ) = await _sync_workspace(
            client=client,
            pool=ctx._pool,
            page_size=page_size,
            updated_after=watermark,
            max_notes=inp.max_notes,
            include_transcripts=inp.include_transcripts,
            run_id=run_id,
        )
        await _update_checkpoint_success(
            ctx._pool,
            scope_id=WORKSPACE_SCOPE,
            watermark_time=successful_watermark,
            run_id=run_id,
        )
        synced.append(_scope_ref(WORKSPACE_SCOPE))
    except Exception as exc:
        error = str(exc)
        failed.append(_scope_ref(WORKSPACE_SCOPE, error))
        record_etl_items_failed(
            "granola", WORKSPACE_SCOPE, "scope", _failure_reason(error)
        )
        await _update_checkpoint_failure(
            ctx._pool,
            scope_id=WORKSPACE_SCOPE,
            run_id=run_id,
            error=error,
        )
        ctx.log("granola_sync_scope_failed", scope_id=WORKSPACE_SCOPE, error=error)

    status = "completed" if not failed else "failed"
    error_text = "" if not failed else "Granola workspace sync failed"
    await _record_run_finish(
        ctx._pool,
        run_id=run_id,
        status=status,
        scopes_synced=synced,
        scopes_failed=failed,
        counts=counts,
        error_text=error_text,
    )
    await _emit_checkpoint_metrics(ctx._pool)

    return {"status": status, "run_id": run_id, **counts}
