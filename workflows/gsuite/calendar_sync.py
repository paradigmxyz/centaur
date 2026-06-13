"""Workflow: sync Google Calendar metadata and events into Postgres."""

from __future__ import annotations

import datetime as dt
import hashlib
import os
from dataclasses import dataclass, field
from typing import Any, Protocol

from api.runtime_control import canonical_json
from api.vm_metrics import (
    record_etl_items_failed,
    record_etl_items_seen,
    record_etl_items_upserted,
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import env_flag_enabled, positive_int

WORKFLOW_NAME = "google_calendar_sync"
DEFAULT_SYNC_INTERVAL_SECONDS = 4 * 60 * 60
DEFAULT_PAGE_SIZE = 250


SCHEDULE = {
    "schedule_id": "google_calendar_sync",
    "interval_seconds": positive_int(
        os.getenv("GOOGLE_CALENDAR_SYNC_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": env_flag_enabled("GOOGLE_CALENDAR_ETL_ENABLED", default=False),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for a manual Google Calendar sync workflow run."""

    calendar_ids: list[str] = field(default_factory=list)
    limit: int = DEFAULT_PAGE_SIZE
    reset_sync: bool = False
    metadata: dict[str, Any] = field(default_factory=dict)


class GoogleCalendarSyncClient(Protocol):
    """Small adapter protocol used by the Calendar ETL workflow."""

    def list_calendars(self, *, page_token: str | None = None) -> dict[str, Any]: ...

    def list_events(
        self,
        *,
        calendar_id: str,
        page_size: int,
        page_token: str | None = None,
        sync_token: str | None = None,
    ) -> dict[str, Any]: ...


def _client() -> GoogleCalendarSyncClient:
    from workflows.gsuite.calendar import GoogleCalendarReadonlyClient

    return GoogleCalendarReadonlyClient()


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


def _event_boundary(payload: Any) -> tuple[dt.datetime | None, bool]:
    if not isinstance(payload, dict):
        return None, False
    if payload.get("dateTime"):
        return _parse_datetime(str(payload.get("dateTime") or "")), False
    if payload.get("date"):
        parsed = _parse_datetime(f"{payload.get('date')}T00:00:00Z")
        return parsed, True
    return None, False


def _content_hash(*parts: Any) -> str:
    return hashlib.sha256(canonical_json(parts).encode("utf-8")).hexdigest()


def _workflow_run_id_to_sync_run_id(workflow_run_id: str) -> str:
    safe_run_id = "".join(char if char.isalnum() else "_" for char in workflow_run_id)
    return f"google_calendar_sync_{safe_run_id}"


def _calendar_ref(calendar_id: str, reason: str | None = None) -> dict[str, str]:
    result = {"calendar_id": calendar_id}
    if reason:
        result["reason"] = reason
    return result


def _is_sync_token_expired(error: Exception) -> bool:
    response = getattr(error, "resp", None) or getattr(error, "response", None)
    status = getattr(response, "status", None) or getattr(response, "status_code", None)
    try:
        numeric_status = int(status or 0)
    except (TypeError, ValueError):
        numeric_status = 0
    return numeric_status == 410 or "410" in str(error)


def _int_value(value: Any, default: int = 0) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _attendee_names(attendees: Any) -> list[str]:
    if not isinstance(attendees, list):
        return []
    names: list[str] = []
    for attendee in attendees:
        if not isinstance(attendee, dict):
            continue
        value = str(attendee.get("displayName") or attendee.get("email") or "").strip()
        if value:
            names.append(value)
    return names


def _event_content_text(calendar: dict[str, Any], event: dict[str, Any]) -> str:
    start_at, _ = _event_boundary(event.get("start"))
    end_at, _ = _event_boundary(event.get("end"))
    parts = [
        str(event.get("summary") or ""),
        str(event.get("description") or ""),
        str(event.get("location") or ""),
        str(calendar.get("summary") or ""),
        " ".join(_attendee_names(event.get("attendees"))),
    ]
    if start_at:
        parts.append(start_at.isoformat())
    if end_at:
        parts.append(end_at.isoformat())
    return "\n".join(part for part in parts if part.strip())


async def _record_run_start(
    pool,
    *,
    run_id: str,
    workflow_run_id: str,
    calendars_requested: list[dict[str, str]],
    metadata: dict[str, Any],
) -> None:
    await pool.execute(
        "INSERT INTO google_calendar_sync_runs ("
        "run_id, workflow_run_id, mode, status, calendars_requested, metadata"
        ") VALUES ($1, $2, 'incremental', 'running', $3::jsonb, $4::jsonb) "
        "ON CONFLICT (run_id) DO UPDATE SET "
        "workflow_run_id = EXCLUDED.workflow_run_id, "
        "status = 'running', "
        "calendars_requested = EXCLUDED.calendars_requested, "
        "calendars_synced = '[]'::jsonb, "
        "calendars_failed = '[]'::jsonb, "
        "calendars_seen = 0, "
        "calendars_upserted = 0, "
        "events_seen = 0, "
        "events_upserted = 0, "
        "events_cancelled = 0, "
        "finished_at = NULL, "
        "error_text = '', "
        "metadata = EXCLUDED.metadata",
        run_id,
        workflow_run_id,
        canonical_json(calendars_requested),
        canonical_json(metadata),
    )


async def _record_run_finish(
    pool,
    *,
    run_id: str,
    status: str,
    calendars_synced: list[dict[str, str]],
    calendars_failed: list[dict[str, str]],
    counts: dict[str, int],
    error_text: str = "",
) -> None:
    await pool.execute(
        "UPDATE google_calendar_sync_runs SET "
        "status = $2, calendars_synced = $3::jsonb, calendars_failed = $4::jsonb, "
        "calendars_seen = $5, calendars_upserted = $6, events_seen = $7, "
        "events_upserted = $8, events_cancelled = $9, "
        "finished_at = NOW(), error_text = $10 "
        "WHERE run_id = $1",
        run_id,
        status,
        canonical_json(calendars_synced),
        canonical_json(calendars_failed),
        counts.get("calendars_seen", 0),
        counts.get("calendars_upserted", 0),
        counts.get("events_seen", 0),
        counts.get("events_upserted", 0),
        counts.get("events_cancelled", 0),
        error_text,
    )


async def _load_checkpoint(pool, calendar_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT sync_token, watermark_time, last_error FROM google_calendar_sync_checkpoints "
        "WHERE calendar_id = $1",
        calendar_id,
    )
    return dict(row) if row else None


async def _update_checkpoint_success(
    pool,
    *,
    calendar_id: str,
    sync_token: str,
    watermark_time: dt.datetime | None,
    run_id: str,
) -> None:
    await pool.execute(
        "INSERT INTO google_calendar_sync_checkpoints ("
        "calendar_id, sync_token, watermark_time, last_run_id, last_success_at, "
        "last_error, updated_at"
        ") VALUES ($1, $2, $3, $4, NOW(), '', NOW()) "
        "ON CONFLICT (calendar_id) DO UPDATE SET "
        "sync_token = EXCLUDED.sync_token, "
        "watermark_time = COALESCE(EXCLUDED.watermark_time, google_calendar_sync_checkpoints.watermark_time), "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_success_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        calendar_id,
        sync_token,
        watermark_time,
        run_id,
    )


async def _update_checkpoint_failure(
    pool,
    *,
    calendar_id: str,
    run_id: str,
    error: str,
) -> None:
    await pool.execute(
        "INSERT INTO google_calendar_sync_checkpoints ("
        "calendar_id, last_run_id, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW()) "
        "ON CONFLICT (calendar_id) DO UPDATE SET "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_error = EXCLUDED.last_error, "
        "updated_at = NOW()",
        calendar_id,
        run_id,
        error,
    )


async def _upsert_calendar(pool, *, calendar: dict[str, Any], run_id: str) -> None:
    await pool.execute(
        "INSERT INTO google_calendar_sync_calendars ("
        "calendar_id, summary, description, location, time_zone, access_role, "
        "is_primary, is_selected, is_hidden, background_color, foreground_color, "
        "raw_payload, source_run_id, last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12::jsonb, $13, NOW(), '', NOW()"
        ") ON CONFLICT (calendar_id) DO UPDATE SET "
        "summary = EXCLUDED.summary, "
        "description = EXCLUDED.description, "
        "location = EXCLUDED.location, "
        "time_zone = EXCLUDED.time_zone, "
        "access_role = EXCLUDED.access_role, "
        "is_primary = EXCLUDED.is_primary, "
        "is_selected = EXCLUDED.is_selected, "
        "is_hidden = EXCLUDED.is_hidden, "
        "background_color = EXCLUDED.background_color, "
        "foreground_color = EXCLUDED.foreground_color, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        str(calendar.get("id") or ""),
        str(calendar.get("summary") or ""),
        str(calendar.get("description") or ""),
        str(calendar.get("location") or ""),
        str(calendar.get("timeZone") or ""),
        str(calendar.get("accessRole") or ""),
        bool(calendar.get("primary")),
        bool(calendar.get("selected")),
        bool(calendar.get("hidden")),
        str(calendar.get("backgroundColor") or ""),
        str(calendar.get("foregroundColor") or ""),
        canonical_json(calendar),
        run_id,
    )


async def _record_calendar_error(
    pool,
    *,
    calendar_id: str,
    error: str,
    run_id: str,
) -> None:
    await pool.execute(
        "UPDATE google_calendar_sync_calendars SET "
        "last_error = $2, source_run_id = $3, updated_at = NOW() "
        "WHERE calendar_id = $1",
        calendar_id,
        error,
        run_id,
    )


async def _upsert_event(
    pool,
    *,
    calendar: dict[str, Any],
    event: dict[str, Any],
    run_id: str,
) -> None:
    calendar_id = str(calendar.get("id") or "")
    start_at, is_all_day = _event_boundary(event.get("start"))
    end_at, _ = _event_boundary(event.get("end"))
    content_text = _event_content_text(calendar, event)
    await pool.execute(
        "INSERT INTO google_calendar_sync_events ("
        "calendar_id, event_id, i_cal_uid, status, summary, description, location, "
        "html_link, creator, organizer, attendees, start_payload, end_payload, "
        "start_at, end_at, is_all_day, recurring_event_id, original_start, "
        "transparency, visibility, event_type, sequence, source_created_at, "
        "source_updated_at, content_text, content_hash, raw_payload, source_run_id, "
        "last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10::jsonb, $11::jsonb, "
        "$12::jsonb, $13::jsonb, $14, $15, $16, $17, $18::jsonb, $19, $20, $21, "
        "$22, $23, $24, $25, $26, $27::jsonb, $28, NOW(), '', NOW()"
        ") ON CONFLICT (calendar_id, event_id) DO UPDATE SET "
        "i_cal_uid = EXCLUDED.i_cal_uid, "
        "status = EXCLUDED.status, "
        "summary = EXCLUDED.summary, "
        "description = EXCLUDED.description, "
        "location = EXCLUDED.location, "
        "html_link = EXCLUDED.html_link, "
        "creator = EXCLUDED.creator, "
        "organizer = EXCLUDED.organizer, "
        "attendees = EXCLUDED.attendees, "
        "start_payload = EXCLUDED.start_payload, "
        "end_payload = EXCLUDED.end_payload, "
        "start_at = EXCLUDED.start_at, "
        "end_at = EXCLUDED.end_at, "
        "is_all_day = EXCLUDED.is_all_day, "
        "recurring_event_id = EXCLUDED.recurring_event_id, "
        "original_start = EXCLUDED.original_start, "
        "transparency = EXCLUDED.transparency, "
        "visibility = EXCLUDED.visibility, "
        "event_type = EXCLUDED.event_type, "
        "sequence = EXCLUDED.sequence, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "content_text = EXCLUDED.content_text, "
        "content_hash = EXCLUDED.content_hash, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        calendar_id,
        str(event.get("id") or ""),
        str(event.get("iCalUID") or ""),
        str(event.get("status") or ""),
        str(event.get("summary") or ""),
        str(event.get("description") or ""),
        str(event.get("location") or ""),
        str(event.get("htmlLink") or ""),
        canonical_json(event.get("creator") if isinstance(event.get("creator"), dict) else {}),
        canonical_json(event.get("organizer") if isinstance(event.get("organizer"), dict) else {}),
        canonical_json(event.get("attendees") if isinstance(event.get("attendees"), list) else []),
        canonical_json(event.get("start") if isinstance(event.get("start"), dict) else {}),
        canonical_json(event.get("end") if isinstance(event.get("end"), dict) else {}),
        start_at,
        end_at,
        is_all_day,
        str(event.get("recurringEventId") or ""),
        canonical_json(
            event.get("originalStartTime")
            if isinstance(event.get("originalStartTime"), dict)
            else {}
        ),
        str(event.get("transparency") or ""),
        str(event.get("visibility") or ""),
        str(event.get("eventType") or ""),
        _int_value(event.get("sequence")),
        _parse_datetime(str(event.get("created") or "")),
        _parse_datetime(str(event.get("updated") or "")),
        content_text,
        _content_hash(content_text),
        canonical_json(event),
        run_id,
    )


async def _list_visible_calendars(client: GoogleCalendarSyncClient) -> list[dict[str, Any]]:
    calendars: list[dict[str, Any]] = []
    page_token: str | None = None
    while True:
        page = client.list_calendars(page_token=page_token)
        for calendar in page.get("items", []) or []:
            if str(calendar.get("id") or ""):
                calendars.append(calendar)
        page_token = page.get("nextPageToken")
        if not page_token:
            break
    return calendars


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Sync Google Calendar calendars and changed events into raw sync tables."""
    if not env_flag_enabled("GOOGLE_CALENDAR_ETL_ENABLED", default=False):
        ctx.log("google_calendar_sync_skipped_disabled")
        return {"status": "skipped", "reason": "google_calendar_etl_disabled"}

    page_size = positive_int(inp.limit, DEFAULT_PAGE_SIZE)
    run_id = _workflow_run_id_to_sync_run_id(ctx.run_id)
    requested_ids = {calendar_id.strip() for calendar_id in inp.calendar_ids if calendar_id.strip()}
    requested = [_calendar_ref(calendar_id) for calendar_id in sorted(requested_ids)]
    if not requested:
        requested = [_calendar_ref("all_visible")]

    await _record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        calendars_requested=requested,
        metadata={
            **inp.metadata,
            "page_size": page_size,
            "reset_sync": inp.reset_sync,
        },
    )

    client = _client()
    synced: list[dict[str, str]] = []
    failed: list[dict[str, str]] = []
    counts = {
        "calendars_seen": 0,
        "calendars_upserted": 0,
        "events_seen": 0,
        "events_upserted": 0,
        "events_cancelled": 0,
    }

    try:
        calendars = await _list_visible_calendars(client)
    except Exception as exc:
        error = str(exc)
        record_etl_items_failed("google_calendar", "calendar", "scope", "api_error")
        await _record_run_finish(
            ctx._pool,
            run_id=run_id,
            status="failed",
            calendars_synced=[],
            calendars_failed=[_calendar_ref("all_visible", error)],
            counts=counts,
            error_text=error,
        )
        ctx.log("google_calendar_sync_list_calendars_failed", error=error)
        return {"status": "failed", "run_id": run_id, **counts}

    by_id = {str(calendar.get("id") or ""): calendar for calendar in calendars}
    if requested_ids:
        calendars_to_sync = []
        for calendar_id in sorted(requested_ids):
            calendars_to_sync.append(by_id.get(calendar_id) or {"id": calendar_id, "summary": calendar_id})
    else:
        calendars_to_sync = calendars

    counts["calendars_seen"] = len(calendars_to_sync)
    record_etl_items_seen(
        "google_calendar", "calendar", "calendar", len(calendars_to_sync)
    )

    for calendar in calendars_to_sync:
        calendar_id = str(calendar.get("id") or "")
        if not calendar_id:
            continue
        await _upsert_calendar(ctx._pool, calendar=calendar, run_id=run_id)
        counts["calendars_upserted"] += 1
        record_etl_items_upserted("google_calendar", "calendar", "calendar", 1)

        checkpoint = await _load_checkpoint(ctx._pool, calendar_id)
        sync_token = "" if inp.reset_sync else str((checkpoint or {}).get("sync_token") or "")
        page_token: str | None = None
        next_sync_token = sync_token
        successful_watermark: dt.datetime | None = None
        retried_full_sync = False
        try:
            while True:
                try:
                    page = client.list_events(
                        calendar_id=calendar_id,
                        page_size=page_size,
                        page_token=page_token,
                        sync_token=sync_token or None,
                    )
                except Exception as exc:
                    if sync_token and not retried_full_sync and _is_sync_token_expired(exc):
                        ctx.log(
                            "google_calendar_sync_token_expired",
                            calendar_id=calendar_id,
                            error=str(exc),
                        )
                        sync_token = ""
                        page_token = None
                        next_sync_token = ""
                        retried_full_sync = True
                        continue
                    raise

                events = [
                    event
                    for event in page.get("items", []) or []
                    if str(event.get("id") or "")
                ]
                counts["events_seen"] += len(events)
                record_etl_items_seen(
                    "google_calendar", "calendar", "event", len(events)
                )
                for event in events:
                    await _upsert_event(
                        ctx._pool,
                        calendar=calendar,
                        event=event,
                        run_id=run_id,
                    )
                    counts["events_upserted"] += 1
                    if str(event.get("status") or "") == "cancelled":
                        counts["events_cancelled"] += 1
                    record_etl_items_upserted("google_calendar", "calendar", "event", 1)
                    updated_at = _parse_datetime(str(event.get("updated") or ""))
                    if updated_at and (
                        successful_watermark is None
                        or updated_at > successful_watermark
                    ):
                        successful_watermark = updated_at

                page_token = page.get("nextPageToken")
                if not page_token:
                    next_sync_token = str(page.get("nextSyncToken") or next_sync_token or "")
                    break

            await _update_checkpoint_success(
                ctx._pool,
                calendar_id=calendar_id,
                sync_token=next_sync_token,
                watermark_time=successful_watermark,
                run_id=run_id,
            )
            synced.append(_calendar_ref(calendar_id))
            ctx.log(
                "google_calendar_sync_calendar_completed",
                calendar_id=calendar_id,
                events_seen=counts["events_seen"],
                events_upserted=counts["events_upserted"],
                sync_token_present=bool(next_sync_token),
            )
        except Exception as exc:
            error = str(exc)
            failed.append(_calendar_ref(calendar_id, error))
            record_etl_items_failed("google_calendar", "calendar", "scope", "api_error")
            await _record_calendar_error(
                ctx._pool,
                calendar_id=calendar_id,
                error=error,
                run_id=run_id,
            )
            await _update_checkpoint_failure(
                ctx._pool,
                calendar_id=calendar_id,
                run_id=run_id,
                error=error,
            )
            ctx.log(
                "google_calendar_sync_calendar_failed",
                calendar_id=calendar_id,
                error=error,
            )

    status = "completed"
    error_text = ""
    if failed and synced:
        status = "partial_failed"
        error_text = f"{len(failed)} calendar(s) failed"
    elif failed:
        status = "failed"
        error_text = f"{len(failed)} calendar(s) failed"

    await _record_run_finish(
        ctx._pool,
        run_id=run_id,
        status=status,
        calendars_synced=synced,
        calendars_failed=failed,
        counts=counts,
        error_text=error_text,
    )

    return {
        "status": status,
        "run_id": run_id,
        "calendars_synced": len(synced),
        "calendars_failed": len(failed),
        **counts,
    }
