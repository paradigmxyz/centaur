"""Workflow: sync Attio meetings and call transcripts into Postgres."""

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
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import env_flag_enabled, positive_int

WORKFLOW_NAME = "attio_sync"
DEFAULT_SYNC_INTERVAL_SECONDS = 4 * 60 * 60
DEFAULT_PAGE_SIZE = 50
DEFAULT_WATERMARK_OVERLAP_SECONDS = 5 * 60
MEETINGS_SCOPE = "meetings"


SCHEDULE = {
    "schedule_id": "attio_sync",
    "interval_seconds": positive_int(
        os.getenv("ATTIO_SYNC_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": env_flag_enabled("ATTIO_ETL_ENABLED", default=False),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for a manual Attio sync workflow run."""

    since: str | None = None
    limit: int = DEFAULT_PAGE_SIZE
    max_meetings: int | None = None
    include_transcripts: bool = True
    watermark_overlap_seconds: int = DEFAULT_WATERMARK_OVERLAP_SECONDS
    metadata: dict[str, Any] = field(default_factory=dict)


class AttioSyncClient(Protocol):
    """Small adapter protocol used by the Attio ETL workflow."""

    async def list_meetings(
        self,
        limit: int = DEFAULT_PAGE_SIZE,
        cursor: str | None = None,
        linked_object: str | None = None,
        linked_record_id: str | None = None,
        participants: list[str] | str | None = None,
        sort: str | None = None,
        ends_from: str | None = None,
        starts_before: str | None = None,
        timezone: str | None = None,
    ) -> dict[str, Any]: ...

    async def get_meeting(self, meeting_id: str) -> dict[str, Any]: ...

    async def list_call_recordings(
        self,
        meeting_id: str,
        limit: int = DEFAULT_PAGE_SIZE,
        cursor: str | None = None,
    ) -> dict[str, Any]: ...

    async def get_call_transcript(
        self,
        meeting_id: str,
        call_recording_id: str,
        cursor: str | None = None,
    ) -> dict[str, Any]: ...


class AttioToolClient:
    """Attio client backed by the workflow tool bridge."""

    def __init__(self, ctx: WorkflowContext) -> None:
        self._ctx = ctx

    async def list_meetings(
        self,
        limit: int = DEFAULT_PAGE_SIZE,
        cursor: str | None = None,
        linked_object: str | None = None,
        linked_record_id: str | None = None,
        participants: list[str] | str | None = None,
        sort: str | None = None,
        ends_from: str | None = None,
        starts_before: str | None = None,
        timezone: str | None = None,
    ) -> dict[str, Any]:
        result = await self._ctx.call_tool(
            "attio",
            "list_meetings",
            {
                "limit": limit,
                "cursor": cursor,
                "linked_object": linked_object,
                "linked_record_id": linked_record_id,
                "participants": participants,
                "sort": sort,
                "ends_from": ends_from,
                "starts_before": starts_before,
                "timezone": timezone,
            },
        )
        return result if isinstance(result, dict) else {}

    async def get_meeting(self, meeting_id: str) -> dict[str, Any]:
        result = await self._ctx.call_tool(
            "attio",
            "get_meeting",
            {"meeting_id": meeting_id},
        )
        return result if isinstance(result, dict) else {}

    async def list_call_recordings(
        self,
        meeting_id: str,
        limit: int = DEFAULT_PAGE_SIZE,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        result = await self._ctx.call_tool(
            "attio",
            "list_call_recordings",
            {"meeting_id": meeting_id, "limit": limit, "cursor": cursor},
        )
        return result if isinstance(result, dict) else {}

    async def get_call_transcript(
        self,
        meeting_id: str,
        call_recording_id: str,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        result = await self._ctx.call_tool(
            "attio",
            "get_call_transcript",
            {
                "meeting_id": meeting_id,
                "call_recording_id": call_recording_id,
                "cursor": cursor,
            },
        )
        return result if isinstance(result, dict) else {}


def _client(ctx: WorkflowContext) -> AttioSyncClient:
    return AttioToolClient(ctx)


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


def _json_object(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _json_array(value: Any) -> list[Any]:
    return value if isinstance(value, list) else []


def _content_hash(*parts: Any) -> str:
    return hashlib.sha256(canonical_json(parts).encode("utf-8")).hexdigest()


def _workflow_run_id_to_sync_run_id(workflow_run_id: str) -> str:
    safe_run_id = "".join(char if char.isalnum() else "_" for char in workflow_run_id)
    return f"attio_sync_{safe_run_id}"


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


def _attio_id(value: Any, *keys: str) -> str:
    if isinstance(value, dict):
        for key in keys:
            if value.get(key):
                return _text_value(value.get(key))
        for nested in ("id", "data"):
            if isinstance(value.get(nested), dict):
                found = _attio_id(value[nested], *keys)
                if found:
                    return found
    return _text_value(value)


def _page_items(page: dict[str, Any]) -> list[dict[str, Any]]:
    data = page.get("data")
    if isinstance(data, list):
        return [item for item in data if isinstance(item, dict)]
    if isinstance(data, dict) and isinstance(data.get("data"), list):
        return [item for item in data["data"] if isinstance(item, dict)]
    for key in ("meetings", "call_recordings", "transcript", "items"):
        value = page.get(key)
        if isinstance(value, list):
            return [item for item in value if isinstance(item, dict)]
    return []


def _next_cursor(page: dict[str, Any]) -> str | None:
    pagination = (
        page.get("pagination") if isinstance(page.get("pagination"), dict) else {}
    )
    meta = page.get("meta") if isinstance(page.get("meta"), dict) else {}
    cursor = (
        page.get("next_cursor")
        or page.get("cursor")
        or pagination.get("next_cursor")
        or pagination.get("nextCursor")
        or meta.get("next_cursor")
        or meta.get("nextCursor")
    )
    return _text_value(cursor).strip() or None


def _person(value: Any) -> tuple[str, str, str]:
    obj = _json_object(value)
    person_id = _attio_id(obj.get("id") or obj, "workspace_member_id", "user_id", "id")
    email = _text_value(obj.get("email") or obj.get("email_address"))
    name = _text_value(obj.get("name") or obj.get("display_name") or email)
    return person_id, name, email


def _transcript_text(transcript: Any) -> str:
    lines: list[str] = []
    for item in _json_array(transcript):
        if not isinstance(item, dict):
            continue
        speaker = _json_object(item.get("speaker") or item.get("participant"))
        speaker_name = (
            _text_value(speaker.get("name") or speaker.get("display_name"))
            or _text_value(item.get("speaker_name"))
            or "Unknown"
        )
        text = _text_value(
            item.get("text") or item.get("content") or item.get("transcript")
        ).strip()
        if text:
            lines.append(f"{speaker_name}: {text}")
    return "\n".join(lines)


async def _load_checkpoint(pool, scope_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT watermark_time, last_error FROM attio_sync_checkpoints "
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
        "INSERT INTO attio_sync_checkpoints ("
        "scope_id, watermark_time, last_run_id, last_success_at, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW(), '', NOW()) "
        "ON CONFLICT (scope_id) DO UPDATE SET "
        "watermark_time = COALESCE(EXCLUDED.watermark_time, "
        "attio_sync_checkpoints.watermark_time), "
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
        "INSERT INTO attio_sync_checkpoints ("
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


async def _record_run_start(
    pool,
    *,
    run_id: str,
    workflow_run_id: str,
    scopes_requested: list[dict[str, str]],
    metadata: dict[str, Any],
) -> None:
    await pool.execute(
        "INSERT INTO attio_sync_runs ("
        "run_id, workflow_run_id, mode, status, scopes_requested, metadata"
        ") VALUES ($1, $2, 'incremental', 'running', $3::jsonb, $4::jsonb) "
        "ON CONFLICT (run_id) DO UPDATE SET "
        "workflow_run_id = EXCLUDED.workflow_run_id, "
        "status = 'running', "
        "scopes_requested = EXCLUDED.scopes_requested, "
        "scopes_synced = '[]'::jsonb, "
        "scopes_failed = '[]'::jsonb, "
        "meetings_seen = 0, "
        "meetings_upserted = 0, "
        "call_recordings_seen = 0, "
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
        "UPDATE attio_sync_runs SET "
        "status = $2, scopes_synced = $3::jsonb, scopes_failed = $4::jsonb, "
        "meetings_seen = $5, meetings_upserted = $6, call_recordings_seen = $7, "
        "transcripts_upserted = $8, finished_at = NOW(), error_text = $9 "
        "WHERE run_id = $1",
        run_id,
        status,
        canonical_json(scopes_synced),
        canonical_json(scopes_failed),
        counts.get("meetings_seen", 0),
        counts.get("meetings_upserted", 0),
        counts.get("call_recordings_seen", 0),
        counts.get("transcripts_upserted", 0),
        error_text,
    )


def _recording_id(recording: dict[str, Any]) -> str:
    return _attio_id(
        recording.get("id") or recording, "call_recording_id", "recording_id", "id"
    )


def _meeting_id(meeting: dict[str, Any]) -> str:
    return _attio_id(meeting.get("id") or meeting, "meeting_id", "id")


async def _load_call_recordings(
    client: AttioSyncClient,
    *,
    meeting_id: str,
    page_size: int,
) -> list[dict[str, Any]]:
    recordings: list[dict[str, Any]] = []
    cursor: str | None = None
    while True:
        page = await client.list_call_recordings(
            meeting_id,
            limit=page_size,
            cursor=cursor,
        )
        items = _page_items(page)
        recordings.extend(items)
        cursor = _next_cursor(page)
        if not cursor:
            break
    return recordings


async def _load_transcript(
    client: AttioSyncClient,
    *,
    meeting_id: str,
    recording_id: str,
) -> list[dict[str, Any]]:
    transcript: list[dict[str, Any]] = []
    cursor: str | None = None
    while True:
        page = await client.get_call_transcript(
            meeting_id,
            recording_id,
            cursor=cursor,
        )
        transcript.extend(_page_items(page))
        cursor = _next_cursor(page)
        if not cursor:
            break
    return transcript


async def _upsert_meeting(
    pool,
    *,
    meeting: dict[str, Any],
    call_recordings: list[dict[str, Any]],
    transcript_payload: list[dict[str, Any]],
    run_id: str,
) -> dt.datetime | None:
    meeting_id = _meeting_id(meeting)
    title = _text_value(
        meeting.get("title") or meeting.get("name") or meeting.get("summary")
    )
    description = _text_value(meeting.get("description") or meeting.get("body"))
    linked_records = _json_array(meeting.get("linked_records"))
    participants = _json_array(meeting.get("participants") or meeting.get("attendees"))
    organizer_id, organizer_name, organizer_email = _person(
        meeting.get("organizer") or meeting.get("created_by") or {}
    )
    transcript_text = _transcript_text(transcript_payload)
    started_at = _source_datetime(meeting, "started_at", "starts_at", "start_time")
    ended_at = _source_datetime(meeting, "ended_at", "ends_at", "end_time")
    source_created_at = _source_datetime(meeting, "created_at", "createdAt")
    source_updated_at = (
        _source_datetime(meeting, "updated_at", "updatedAt", "modified_at")
        or ended_at
        or started_at
        or source_created_at
    )
    call_recording_ids = [
        recording_id
        for recording in call_recordings
        if (recording_id := _recording_id(recording))
    ]
    content_text = "\n".join(
        part for part in (title, description, transcript_text) if part.strip()
    )
    await pool.execute(
        "INSERT INTO attio_sync_meetings ("
        "meeting_id, title, description, url, linked_records, participants, "
        "organizer_id, organizer_name, organizer_email, call_recording_ids, "
        "transcript_text, transcript_payload, content_text, content_hash, "
        "started_at, ended_at, source_created_at, source_updated_at, raw_payload, "
        "source_run_id, last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5::jsonb, $6::jsonb, $7, $8, $9, $10::jsonb, "
        "$11, $12::jsonb, $13, $14, $15, $16, $17, $18, $19::jsonb, $20, "
        "NOW(), '', NOW()"
        ") ON CONFLICT (meeting_id) DO UPDATE SET "
        "title = EXCLUDED.title, "
        "description = EXCLUDED.description, "
        "url = EXCLUDED.url, "
        "linked_records = EXCLUDED.linked_records, "
        "participants = EXCLUDED.participants, "
        "organizer_id = EXCLUDED.organizer_id, "
        "organizer_name = EXCLUDED.organizer_name, "
        "organizer_email = EXCLUDED.organizer_email, "
        "call_recording_ids = EXCLUDED.call_recording_ids, "
        "transcript_text = EXCLUDED.transcript_text, "
        "transcript_payload = EXCLUDED.transcript_payload, "
        "content_text = EXCLUDED.content_text, "
        "content_hash = EXCLUDED.content_hash, "
        "started_at = EXCLUDED.started_at, "
        "ended_at = EXCLUDED.ended_at, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        meeting_id,
        title,
        description,
        _text_value(meeting.get("url") or meeting.get("web_url")),
        canonical_json(linked_records),
        canonical_json(participants),
        organizer_id,
        organizer_name,
        organizer_email,
        canonical_json(call_recording_ids),
        transcript_text,
        canonical_json(transcript_payload),
        content_text,
        _content_hash(content_text),
        started_at,
        ended_at,
        source_created_at,
        source_updated_at,
        canonical_json(meeting),
        run_id,
    )
    return source_updated_at


async def _sync_meetings(
    *,
    client: AttioSyncClient,
    pool,
    page_size: int,
    updated_after: dt.datetime | None,
    max_meetings: int | None,
    include_transcripts: bool,
    run_id: str,
) -> tuple[int, int, int, int, dt.datetime | None]:
    seen = 0
    upserted = 0
    recordings_seen = 0
    transcripts_upserted = 0
    watermark: dt.datetime | None = None
    cursor: str | None = None
    ends_from = _rfc3339(updated_after) if updated_after else None

    while True:
        page = await client.list_meetings(
            limit=page_size,
            cursor=cursor,
            ends_from=ends_from,
            sort="start_asc",
        )
        meetings = [meeting for meeting in _page_items(page) if _meeting_id(meeting)]
        if max_meetings is not None:
            meetings = meetings[: max(max_meetings - seen, 0)]
        seen += len(meetings)
        record_etl_items_seen("attio", MEETINGS_SCOPE, "meeting", len(meetings))

        for meeting_ref in meetings:
            meeting_id = _meeting_id(meeting_ref)
            meeting = await client.get_meeting(meeting_id)
            if not isinstance(meeting, dict) or not meeting:
                meeting = meeting_ref
            meeting.setdefault("id", {"meeting_id": meeting_id})
            call_recordings: list[dict[str, Any]] = []
            transcript_payload: list[dict[str, Any]] = []
            if include_transcripts:
                call_recordings = await _load_call_recordings(
                    client,
                    meeting_id=meeting_id,
                    page_size=page_size,
                )
                recordings_seen += len(call_recordings)
                for recording in call_recordings:
                    recording_id = _recording_id(recording)
                    if not recording_id:
                        continue
                    transcript_payload.extend(
                        await _load_transcript(
                            client,
                            meeting_id=meeting_id,
                            recording_id=recording_id,
                        )
                    )
                if transcript_payload:
                    transcripts_upserted += 1
                    record_etl_items_upserted("attio", MEETINGS_SCOPE, "transcript", 1)

            source_updated_at = await _upsert_meeting(
                pool,
                meeting=meeting,
                call_recordings=call_recordings,
                transcript_payload=transcript_payload,
                run_id=run_id,
            )
            upserted += 1
            record_etl_items_upserted("attio", MEETINGS_SCOPE, "meeting", 1)
            if source_updated_at and (
                watermark is None or source_updated_at > watermark
            ):
                watermark = source_updated_at

        if max_meetings is not None and seen >= max_meetings:
            break
        cursor = _next_cursor(page)
        if not cursor:
            break

    return seen, upserted, recordings_seen, transcripts_upserted, watermark


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Sync changed Attio meetings into raw sync tables."""
    if not env_flag_enabled("ATTIO_ETL_ENABLED", default=False):
        ctx.log("attio_sync_skipped_disabled")
        return {"status": "skipped", "reason": "attio_etl_disabled"}

    page_size = positive_int(inp.limit, DEFAULT_PAGE_SIZE)
    overlap_seconds = max(int(inp.watermark_overlap_seconds), 0)
    run_id = _workflow_run_id_to_sync_run_id(ctx.run_id)
    scopes_requested = [_scope_ref(MEETINGS_SCOPE)]

    await _record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        scopes_requested=scopes_requested,
        metadata={
            **inp.metadata,
            "page_size": page_size,
            "max_meetings": inp.max_meetings,
            "include_transcripts": inp.include_transcripts,
        },
    )

    client = _client(ctx)
    explicit_since = _parse_datetime(inp.since)
    checkpoint = await _load_checkpoint(ctx._pool, MEETINGS_SCOPE)
    watermark = explicit_since
    if watermark is None and checkpoint and checkpoint.get("watermark_time"):
        watermark = checkpoint["watermark_time"].astimezone(dt.timezone.utc)
    if watermark is not None:
        watermark = watermark - dt.timedelta(seconds=overlap_seconds)

    synced: list[dict[str, str]] = []
    failed: list[dict[str, str]] = []
    counts = {
        "meetings_seen": 0,
        "meetings_upserted": 0,
        "call_recordings_seen": 0,
        "transcripts_upserted": 0,
    }
    try:
        (
            counts["meetings_seen"],
            counts["meetings_upserted"],
            counts["call_recordings_seen"],
            counts["transcripts_upserted"],
            successful_watermark,
        ) = await _sync_meetings(
            client=client,
            pool=ctx._pool,
            page_size=page_size,
            updated_after=watermark,
            max_meetings=inp.max_meetings,
            include_transcripts=inp.include_transcripts,
            run_id=run_id,
        )
        await _update_checkpoint_success(
            ctx._pool,
            scope_id=MEETINGS_SCOPE,
            watermark_time=successful_watermark,
            run_id=run_id,
        )
        synced.append(_scope_ref(MEETINGS_SCOPE))
    except Exception as exc:
        error = str(exc)
        failed.append(_scope_ref(MEETINGS_SCOPE, error))
        record_etl_items_failed(
            "attio", MEETINGS_SCOPE, "scope", _failure_reason(error)
        )
        await _update_checkpoint_failure(
            ctx._pool,
            scope_id=MEETINGS_SCOPE,
            run_id=run_id,
            error=error,
        )
        ctx.log("attio_sync_scope_failed", scope_id=MEETINGS_SCOPE, error=error)

    status = "completed" if not failed else "failed"
    error_text = "" if not failed else "Attio meetings sync failed"
    await _record_run_finish(
        ctx._pool,
        run_id=run_id,
        status=status,
        scopes_synced=synced,
        scopes_failed=failed,
        counts=counts,
        error_text=error_text,
    )

    return {"status": status, "run_id": run_id, **counts}
