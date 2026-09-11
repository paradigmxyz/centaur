"""Calendar + Zoom scheduling operations for Centaur workflows.

The tool deliberately does not expose arbitrary Calendar writes. Calendar IDs
are resolved from a managed organizer alias, participant availability is read
through free/busy only, and every write carries a stable Centaur occurrence key.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import hashlib
import json
import os
import re
import secrets
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any
from urllib.parse import quote, urljoin, urlparse, urlunparse
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

import asyncpg
import httplib2
import httpx
import socks
from googleapiclient.discovery import build

from centaur_sdk.tool_sdk import secret

try:
    from api.integrations.gsuite.http import build_http as _shared_build_http
except ModuleNotFoundError:
    _shared_build_http = None

POSTGRES_DSN = "CENTAUR_POSTGRES_DSN"
ZOOM_ACCESS_TOKEN = "ZOOM_ACCESS_TOKEN"
SCHEDULER_ENABLED = "MEETING_SCHEDULER_ENABLED"
ORGANIZER_CALENDARS = "MEETING_ORGANIZER_CALENDARS"
ZOOM_HOST_USER_ID = "MEETING_ZOOM_HOST_USER_ID"
DEFAULT_DATABASE = "ai_v2"
DEFAULT_TIME_ZONE = "UTC"
MAX_CANDIDATES = 32
SCHEDULER_STATUSES = {"pending", "booked", "blocked", "completed", "cancelled"}
EMAIL_RE = re.compile(r"^[^@\s<>]+@[^@\s<>]+\.[^@\s<>]+$")
WRITABLE_CALENDAR_ACCESS_ROLES = frozenset({"writer", "owner"})
MAX_TRANSCRIPT_BYTES = 10 * 1024 * 1024
ZOOM_TRANSCRIPT_DOWNLOAD_TIMEOUT_SECONDS = 30.0
MAX_MEETING_ID_LENGTH = 128
MAX_POST_MEETING_ERROR_LENGTH = 2000
MAX_POST_MEETING_STATE_LENGTH = 64
MAX_POST_MEETING_EVENT_LENGTH = 128
MAX_ZOOM_ERROR_DETAIL_LENGTH = 400
MAX_ZOOM_ERROR_FIELD_ERRORS = 5
# Zoom error bodies are {"code": int, "message": str, "errors": [{"field", "message"}]}.
# Only those fields are retained, after removing anything that could carry a
# credential, a signed URL, or an identity: URLs and zoom.us hosts, bearer
# tokens, JWT-shaped or long opaque tokens, email addresses, and control
# characters. Headers and unrecognized body content are never retained.
_ZOOM_ERROR_REDACTIONS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"(?i)\b(?:https?|wss?)://[^\s<>\"']+"), "[url]"),
    (re.compile(r"(?i)\b[\w.-]*zoom\.us[^\s<>\"']*"), "[url]"),
    (re.compile(r"(?i)\bbearer\s+[^\s]+"), "[token]"),
    (re.compile(r"\beyJ[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]+)*"), "[token]"),
    (re.compile(r"\b[A-Za-z0-9_-]{32,}\b"), "[token]"),
    (re.compile(r"[^\s@<>\"']+@[^\s@<>\"']+\.[^\s@<>\"']+"), "[email]"),
    (re.compile(r"[\x00-\x1f\x7f]+"), " "),
)


class MeetingSchedulerError(RuntimeError):
    """Raised for fail-closed scheduling or provider errors."""


def _zoom_occurrence_marker(key: str) -> str:
    """Return an opaque retry marker accepted by Zoom's free-form agenda."""

    return f"centaur-occurrence:{hashlib.sha256(key.encode('utf-8')).hexdigest()[:32]}"


def _redact_zoom_error_text(value: str, limit: int) -> str:
    """Bound one Zoom error string and strip anything secret-shaped."""

    text = value
    for pattern, replacement in _ZOOM_ERROR_REDACTIONS:
        text = pattern.sub(replacement, text)
    text = " ".join(text.split())
    if len(text) > limit:
        text = text[: limit - 1].rstrip() + "…"
    return text


def _zoom_error_detail(response: httpx.Response) -> str:
    """Return a bounded, redacted summary of a Zoom error body.

    The result distinguishes scope, host identity, account policy, and payload
    validation failures for durable occurrence state and operator logs while
    never retaining headers, URLs, tokens, or arbitrary response content.
    """

    if not response.content:
        return ""
    try:
        body = response.json()
    except ValueError:
        return ""
    if not isinstance(body, dict):
        return ""
    parts: list[str] = []
    code = body.get("code")
    if isinstance(code, (int, str)) and not isinstance(code, bool):
        code_text = _redact_zoom_error_text(str(code), 32)
        if code_text:
            parts.append(f"zoom code {code_text}")
    message = body.get("message")
    if isinstance(message, str):
        message_text = _redact_zoom_error_text(message, MAX_ZOOM_ERROR_DETAIL_LENGTH)
        if message_text:
            parts.append(message_text)
    detail = ": ".join(parts)
    errors = body.get("errors")
    field_parts: list[str] = []
    for item in (errors if isinstance(errors, list) else [])[:MAX_ZOOM_ERROR_FIELD_ERRORS]:
        if not isinstance(item, dict):
            continue
        field = _redact_zoom_error_text(str(item.get("field") or ""), 64)
        field_message = _redact_zoom_error_text(str(item.get("message") or ""), 120)
        if field and field_message:
            field_parts.append(f"{field}: {field_message}")
        elif field or field_message:
            field_parts.append(field or field_message)
    if field_parts:
        detail = f"{detail} [{'; '.join(field_parts)}]".strip()
    if len(detail) > MAX_ZOOM_ERROR_DETAIL_LENGTH:
        detail = detail[: MAX_ZOOM_ERROR_DETAIL_LENGTH - 1].rstrip() + "…"
    return detail


@dataclass(frozen=True)
class _OperationFailure:
    error: Exception


def _secret_value(name: str, default: str = "") -> str:
    """Read operator config while treating server-mode placeholders as absent."""

    value = secret(name, default)
    return default if value in (None, "", name) else value


def _config_value(name: str, default: str = "") -> str:
    """Read non-secret deployment configuration from the sandbox environment."""

    try:
        return os.environ[name]
    except KeyError:
        return default


def _enabled() -> bool:
    value = _config_value(SCHEDULER_ENABLED, "false").strip().lower()
    return value not in {"0", "false", "no", "off"}


def _require_enabled() -> None:
    if not _enabled():
        raise MeetingSchedulerError("meeting scheduling is disabled")


def _build_http() -> httplib2.Http:
    if _shared_build_http is not None:
        return _shared_build_http()
    proxy_url = _config_value("HTTPS_PROXY") or _config_value("https_proxy")
    proxy_info = None
    if proxy_url:
        parts = urlparse(proxy_url)
        proxy_info = httplib2.ProxyInfo(
            proxy_type=socks.PROXY_TYPE_HTTP,
            proxy_host=parts.hostname,
            proxy_port=parts.port or 8080,
        )
    ca_certs = _config_value("SSL_CERT_FILE") or _config_value("REQUESTS_CA_BUNDLE")
    return httplib2.Http(proxy_info=proxy_info, ca_certs=ca_certs)


def get_calendar_service():
    return build("calendar", "v3", http=_build_http())


def _parse_rfc3339(value: str, *, field: str) -> dt.datetime:
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise MeetingSchedulerError(f"{field} must be an RFC3339 timestamp") from error
    if parsed.tzinfo is None:
        raise MeetingSchedulerError(f"{field} must include a timezone offset")
    return parsed.astimezone(dt.UTC)


def _rfc3339(value: dt.datetime) -> str:
    return value.astimezone(dt.UTC).isoformat().replace("+00:00", "Z")


def _rfc3339_in_zone(value: dt.datetime, time_zone: str) -> str:
    localized = value.astimezone(_zone(time_zone))
    if localized.utcoffset() == dt.timedelta(0):
        return _rfc3339(localized)
    return localized.isoformat()


def _positive_int(value: Any, field: str) -> int:
    try:
        number = int(value)
    except (TypeError, ValueError) as error:
        raise MeetingSchedulerError(f"{field} must be a positive integer") from error
    if number <= 0:
        raise MeetingSchedulerError(f"{field} must be a positive integer")
    return number


def _email_list(values: Any) -> list[str]:
    if isinstance(values, str):
        values = [item.strip() for item in values.replace(";", ",").split(",")]
    if not isinstance(values, list):
        raise MeetingSchedulerError("attendee_emails must be a list of exact email addresses")
    normalized: list[str] = []
    for value in values:
        email = str(value or "").strip().lower()
        if not EMAIL_RE.fullmatch(email):
            raise MeetingSchedulerError("attendee_emails must contain exact email addresses")
        if email not in normalized:
            normalized.append(email)
    if not normalized:
        raise MeetingSchedulerError("at least one attendee email is required")
    return normalized


def _organizer_calendar_map() -> dict[str, str]:
    raw = _config_value(ORGANIZER_CALENDARS, "{}").strip()
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise MeetingSchedulerError(f"{ORGANIZER_CALENDARS} must be a JSON object") from error
    if not isinstance(value, dict):
        raise MeetingSchedulerError(f"{ORGANIZER_CALENDARS} must be a JSON object")
    return {
        str(key): str(calendar_id) for key, calendar_id in value.items() if str(calendar_id).strip()
    }


def _resolve_organizer(alias: str) -> str:
    alias = str(alias or "").strip()
    calendar_id = _organizer_calendar_map().get(alias)
    if calendar_id:
        return calendar_id
    if not EMAIL_RE.fullmatch(alias):
        raise MeetingSchedulerError(f"organizer calendar {alias!r} is not allowlisted")

    # Manual Slack scheduling supplies the verified proposer's email after the
    # workflow has resolved it from Slack. Require an exact, writable calendar
    # match before allowing that identity to become the event organizer.
    try:
        calendars = (
            get_calendar_service().calendarList().list(showHidden=False).execute().get("items", [])
        )
    except Exception as error:
        raise MeetingSchedulerError("Google Calendar organizer lookup failed") from error
    matching = next(
        (
            calendar
            for calendar in calendars
            if isinstance(calendar, dict)
            and str(calendar.get("id") or "").strip().lower() == alias.lower()
        ),
        None,
    )
    if matching is None:
        raise MeetingSchedulerError(f"organizer calendar {alias!r} is not visible to Centaur")
    access_role = str(matching.get("accessRole") or "").strip().lower()
    if access_role not in WRITABLE_CALENDAR_ACCESS_ROLES:
        raise MeetingSchedulerError(f"organizer calendar {alias!r} requires writer or owner access")
    return str(matching["id"])


def _database_url() -> str:
    value = _secret_value(POSTGRES_DSN)
    value = value.strip()
    if not value or value == POSTGRES_DSN:
        raise MeetingSchedulerError(f"{POSTGRES_DSN} is required")
    parsed = urlparse(value)
    if parsed.scheme and parsed.netloc and parsed.path in ("", "/"):
        database = _config_value("MEETING_SCHEDULER_POSTGRES_DATABASE", DEFAULT_DATABASE)
        value = urlunparse(parsed._replace(path=f"/{database}"))
    return value


async def _with_connection(
    operation: Callable[[asyncpg.Connection], Awaitable[Any]],
) -> Any:
    connection = await asyncpg.connect(_database_url(), command_timeout=30)
    try:
        return await operation(connection)
    finally:
        await connection.close()


async def _with_occurrence_lock(
    key: str,
    operation: Callable[[asyncpg.Connection], Awaitable[Any]],
) -> Any:
    """Run one occurrence operation under a cross-replica transaction lock."""

    connection = await asyncpg.connect(_database_url(), command_timeout=30)
    try:
        async with connection.transaction():
            await connection.execute("select pg_advisory_xact_lock(hashtextextended($1, 0))", key)
            return await operation(connection)
    finally:
        await connection.close()


def _serialize_row(row: asyncpg.Record | None) -> dict[str, Any] | None:
    if row is None:
        return None
    result = dict(row)
    for key, value in result.items():
        if isinstance(value, (dt.datetime, dt.date)):
            result[key] = value.isoformat()
    metadata = result.get("metadata")
    if isinstance(metadata, str):
        try:
            decoded_metadata = json.loads(metadata)
        except json.JSONDecodeError:
            decoded_metadata = None
        if isinstance(decoded_metadata, dict):
            result["metadata"] = decoded_metadata
    return result


def _require_occurrence_key(value: Any) -> str:
    key = str(value or "").strip()
    if not key or len(key) > 240 or any(char.isspace() for char in key):
        raise MeetingSchedulerError("occurrence_key must be a non-empty, bounded token")
    return key


def _require_zoom_meeting_id(value: Any) -> str:
    meeting_id = str(value or "").strip()
    if (
        not meeting_id
        or len(meeting_id) > MAX_MEETING_ID_LENGTH
        or any(char.isspace() for char in meeting_id)
    ):
        raise MeetingSchedulerError("meeting_id must be a non-empty, bounded token")
    return meeting_id


def _zoom_meeting_path_id(value: Any) -> str:
    """Encode a Zoom meeting ID or completed-occurrence UUID for a URL path."""

    meeting_id = _require_zoom_meeting_id(value)
    encoded = quote(meeting_id, safe="")
    # Zoom requires a second encoding pass when a completed occurrence UUID
    # starts with '/' or contains '//'. Without it the router decodes the slash
    # before resolving the past meeting and responds with code 300.
    if meeting_id.startswith("/") or "//" in meeting_id:
        encoded = quote(encoded, safe="")
    return encoded


def _require_post_meeting_text(value: Any, *, field: str, limit: int) -> str:
    text = str(value or "").strip()
    if not text or len(text) > limit or any(ord(char) < 32 for char in text):
        raise MeetingSchedulerError(f"{field} must be non-empty and bounded")
    return text


def _slot_confirmation_token(
    *,
    start: dt.datetime,
    duration: int,
    time_zone: str,
    attendees: list[str],
    organizer_calendar_key: str,
    visibility: str | None = None,
) -> str:
    """Bind an ad-hoc write receipt to the exact slot and attendee set."""

    fields: dict[str, Any] = {
        "attendees": sorted(set(attendees)),
        "duration": duration,
        "organizer": organizer_calendar_key,
        "start": _rfc3339(start),
        "time_zone": time_zone,
    }
    if visibility is not None:
        fields["visibility"] = visibility
    payload = json.dumps(
        fields,
        separators=(",", ":"),
        sort_keys=True,
    )
    return f"slot-v1:{hashlib.sha256(payload.encode('utf-8')).hexdigest()}"


def _cancel_confirmation_token(*, occurrence_key: str, organizer_calendar_key: str) -> str:
    payload = f"cancel-v1:{organizer_calendar_key}:{occurrence_key}"
    return f"cancel-v1:{hashlib.sha256(payload.encode('utf-8')).hexdigest()}"


def _end_confirmation_token(*, occurrence_key: str, organizer_calendar_key: str) -> str:
    payload = f"end-v1:{organizer_calendar_key}:{occurrence_key}"
    return f"end-v1:{hashlib.sha256(payload.encode('utf-8')).hexdigest()}"


def _matches_confirmation(actual: str | None, expected: str) -> bool:
    return bool(actual) and secrets.compare_digest(str(actual).strip(), expected)


def _zone(value: str) -> ZoneInfo:
    name = str(value or DEFAULT_TIME_ZONE).strip()
    try:
        return ZoneInfo(name)
    except ZoneInfoNotFoundError as error:
        raise MeetingSchedulerError(f"unknown time zone {name!r}") from error


def _clock_minutes(value: str, *, field: str) -> int:
    try:
        hour, minute = (int(item) for item in str(value).split(":", 1))
    except (TypeError, ValueError) as error:
        raise MeetingSchedulerError(f"{field} must use HH:MM") from error
    if not (0 <= hour <= 23 and 0 <= minute <= 59):
        raise MeetingSchedulerError(f"{field} must use valid HH:MM values")
    return hour * 60 + minute


def _busy_intervals(response: dict[str, Any]) -> list[tuple[dt.datetime, dt.datetime]]:
    intervals: list[tuple[dt.datetime, dt.datetime]] = []
    for calendar in (response.get("calendars") or {}).values():
        if not isinstance(calendar, dict):
            continue
        for busy in calendar.get("busy") or []:
            try:
                start = _parse_rfc3339(str(busy["start"]), field="busy.start")
                end = _parse_rfc3339(str(busy["end"]), field="busy.end")
            except (KeyError, MeetingSchedulerError):
                continue
            if end > start:
                intervals.append((start, end))
    return intervals


class MeetingSchedulerClient:
    """Narrow provider client; public methods are tool methods."""

    def _calendar_ids(
        self, organizer_calendar_key: str, attendee_emails: list[str]
    ) -> tuple[str, list[str]]:
        organizer_id = _resolve_organizer(organizer_calendar_key)
        calendars = list(dict.fromkeys([organizer_id, *attendee_emails]))
        return organizer_id, calendars

    def _freebusy(
        self,
        *,
        calendar_ids: list[str],
        time_min: str,
        time_max: str,
    ) -> dict[str, Any]:
        service = get_calendar_service()
        try:
            response = (
                service.freebusy()
                .query(
                    body={
                        "timeMin": time_min,
                        "timeMax": time_max,
                        "items": [{"id": calendar_id} for calendar_id in calendar_ids],
                    }
                )
                .execute()
            )
        except Exception as error:
            raise MeetingSchedulerError("Google Calendar free/busy query failed") from error
        calendars = response.get("calendars") if isinstance(response, dict) else None
        if not isinstance(calendars, dict):
            raise MeetingSchedulerError("Google Calendar returned no free/busy calendars")
        for calendar_id in calendar_ids:
            calendar = calendars.get(calendar_id)
            if not isinstance(calendar, dict) or calendar.get("errors"):
                raise MeetingSchedulerError(
                    "free/busy access is unavailable for an approved calendar"
                )
        return response

    def _assert_slot_free(
        self,
        *,
        organizer_id: str,
        attendee_emails: list[str],
        start: dt.datetime,
        duration: int,
    ) -> None:
        response = self._freebusy(
            calendar_ids=list(dict.fromkeys([organizer_id, *attendee_emails])),
            time_min=_rfc3339(start),
            time_max=_rfc3339(start + dt.timedelta(minutes=duration)),
        )
        if _busy_intervals(response):
            raise MeetingSchedulerError("requested meeting slot is no longer free")

    def find_availability(
        self,
        organizer_calendar_key: str,
        attendee_emails: list[str],
        time_min: str,
        time_max: str,
        duration_minutes: int,
        response_timezone: str = DEFAULT_TIME_ZONE,
        working_start: str = "09:00",
        working_end: str = "17:00",
    ) -> dict[str, Any]:
        """Return free/busy-derived candidate slots without event details."""
        _require_enabled()
        attendees = _email_list(attendee_emails)
        duration = _positive_int(duration_minutes, "duration_minutes")
        start = _parse_rfc3339(time_min, field="time_min")
        end = _parse_rfc3339(time_max, field="time_max")
        if end <= start:
            raise MeetingSchedulerError("time_max must be after time_min")
        zone = _zone(response_timezone)
        working_start_minutes = _clock_minutes(working_start, field="working_start")
        working_end_minutes = _clock_minutes(working_end, field="working_end")
        if working_end_minutes <= working_start_minutes:
            raise MeetingSchedulerError("working_end must be after working_start")
        _organizer_id, calendars = self._calendar_ids(organizer_calendar_key, attendees)
        freebusy = self._freebusy(
            calendar_ids=calendars, time_min=_rfc3339(start), time_max=_rfc3339(end)
        )
        busy = _busy_intervals(freebusy)
        slot = start
        candidates: list[dict[str, str]] = []
        step = dt.timedelta(minutes=15)
        length = dt.timedelta(minutes=duration)
        while slot + length <= end and len(candidates) < MAX_CANDIDATES:
            candidate_end = slot + length
            local_start = slot.astimezone(zone)
            local_end = candidate_end.astimezone(zone)
            local_start_minutes = local_start.hour * 60 + local_start.minute
            local_end_minutes = local_end.hour * 60 + local_end.minute
            in_working_hours = (
                local_start_minutes >= working_start_minutes
                and local_end_minutes <= working_end_minutes
                and local_start.date() == local_end.date()
            )
            conflict = any(
                slot < busy_end and candidate_end > busy_start for busy_start, busy_end in busy
            )
            if in_working_hours and not conflict:
                candidates.append(
                    {
                        "start": _rfc3339_in_zone(slot, response_timezone),
                        "end": _rfc3339_in_zone(candidate_end, response_timezone),
                        "timezone": response_timezone,
                        "confirmationToken": _slot_confirmation_token(
                            start=slot,
                            duration=duration,
                            time_zone=response_timezone,
                            attendees=attendees,
                            organizer_calendar_key=organizer_calendar_key,
                        ),
                    }
                )
            slot += step
        return {"status": "ok", "candidates": candidates, "calendar_count": len(calendars)}

    async def _get_occurrence(
        self, connection: asyncpg.Connection, key: str
    ) -> dict[str, Any] | None:
        return _serialize_row(
            await connection.fetchrow(
                "select * from centaur_meeting_occurrences where occurrence_key = $1",
                key,
            )
        )

    async def _claim_occurrence_row(
        self,
        connection: asyncpg.Connection,
        *,
        key: str,
        cadence_id: str | None,
        request_id: str | None,
        title: str,
        requested_start: dt.datetime,
        duration: int,
        time_zone: str,
        organizer_key: str,
        organizer_id: str,
        attendees: list[str],
        allow_parameter_update: bool,
        visibility: str = "public",
    ) -> tuple[dict[str, Any], bool]:
        inserted = await connection.fetchrow(
            """
            insert into centaur_meeting_occurrences
                (occurrence_key, cadence_id, request_id, title, requested_start,
                 duration_minutes, time_zone, organizer_calendar_key,
                 organizer_calendar_id, attendee_emails, metadata)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb)
            on conflict (occurrence_key) do nothing
            returning occurrence_key
            """,
            key,
            cadence_id,
            request_id,
            title,
            requested_start,
            duration,
            time_zone,
            organizer_key,
            organizer_id,
            attendees,
            json.dumps({"visibility": visibility}),
        )
        row = await connection.fetchrow(
            "select * from centaur_meeting_occurrences where occurrence_key = $1 for update",
            key,
        )
        if row is None:
            raise MeetingSchedulerError("failed to create occurrence state")
        current = _serialize_row(row) or {}
        if not inserted:
            stored_organizer_key = str(current.get("organizer_calendar_key") or "").strip()
            stored_organizer_id = str(current.get("organizer_calendar_id") or "").strip()
            if stored_organizer_key and stored_organizer_key != organizer_key:
                raise MeetingSchedulerError("occurrence identity does not match organizer calendar")
            if stored_organizer_id and stored_organizer_id != organizer_id:
                raise MeetingSchedulerError("occurrence identity does not match organizer calendar")
        stored_attendees = [
            str(item).strip().lower()
            for item in (current.get("attendee_emails") or [])
            if str(item).strip()
        ]
        stored_start = current.get("requested_start")
        stored_metadata = current.get("metadata")
        stored_metadata = stored_metadata if isinstance(stored_metadata, dict) else {}
        stored_visibility = str(stored_metadata.get("visibility") or "public")
        start_matches = False
        if stored_start:
            try:
                start_matches = (
                    _parse_rfc3339(str(stored_start), field="requested_start") == requested_start
                )
            except MeetingSchedulerError:
                start_matches = False
        parameters_match = (
            str(current.get("title") or "") == title
            and start_matches
            and int(current.get("duration_minutes") or 0) == duration
            and str(current.get("time_zone") or "") == time_zone
            and stored_attendees == attendees
            and stored_visibility == visibility
        )
        if not parameters_match and (
            not allow_parameter_update or current.get("status") in {"completed", "cancelled"}
        ):
            raise MeetingSchedulerError("occurrence parameters cannot be changed by a retry")
        if current.get("status") not in {"booked", "completed", "cancelled"}:
            await connection.execute(
                """
                update centaur_meeting_occurrences
                set title = $2, requested_start = $3, duration_minutes = $4,
                    time_zone = $5, attendee_emails = $6, status = 'pending',
                    metadata = metadata || $7::jsonb,
                    last_error = '', updated_at = now()
                where occurrence_key = $1
                """,
                key,
                title,
                requested_start,
                duration,
                time_zone,
                attendees,
                json.dumps({"visibility": visibility}),
            )
            current.update(
                {
                    "title": title,
                    "requested_start": requested_start.isoformat(),
                    "duration_minutes": duration,
                    "time_zone": time_zone,
                    "attendee_emails": attendees,
                    "metadata": {**stored_metadata, "visibility": visibility},
                    "last_error": "",
                }
            )
            current["status"] = "pending"
        return current, inserted is not None

    async def _claim_occurrence(
        self,
        *,
        key: str,
        cadence_id: str | None,
        request_id: str | None,
        title: str,
        requested_start: dt.datetime,
        duration: int,
        time_zone: str,
        organizer_key: str,
        organizer_id: str,
        attendees: list[str],
    ) -> dict[str, Any]:
        async def claim(connection: asyncpg.Connection) -> dict[str, Any]:
            async with connection.transaction():
                await connection.execute(
                    """
                    insert into centaur_meeting_occurrences
                        (occurrence_key, cadence_id, request_id, title, requested_start,
                         duration_minutes, time_zone, organizer_calendar_key,
                         organizer_calendar_id, attendee_emails)
                    values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    on conflict (occurrence_key) do nothing
                    """,
                    key,
                    cadence_id,
                    request_id,
                    title,
                    requested_start,
                    duration,
                    time_zone,
                    organizer_key,
                    organizer_id,
                    attendees,
                )
                row = await connection.fetchrow(
                    "select * from centaur_meeting_occurrences where occurrence_key = $1 for update",
                    key,
                )
                if row is None:
                    raise MeetingSchedulerError("failed to create occurrence state")
                current = _serialize_row(row) or {}
                if current.get("status") in {"booked", "completed", "cancelled"}:
                    return current
                await connection.execute(
                    "update centaur_meeting_occurrences set status = 'pending', updated_at = now() where occurrence_key = $1",
                    key,
                )
                current["status"] = "pending"
                return current

        return await _with_connection(claim)

    async def _update_provider_state(self, key: str, **values: str) -> dict[str, Any] | None:
        allowed = {
            "zoom_meeting_id": values.get("zoom_id"),
            "zoom_join_url": values.get("join_url"),
            "calendar_event_id": values.get("event_id"),
            "calendar_html_link": values.get("event_link"),
        }
        assignments: list[str] = []
        args: list[Any] = [key]
        for column, value in allowed.items():
            if value is not None:
                args.append(value)
                assignments.append(f"{column} = ${len(args)}")
        if not assignments:
            return await self._get_existing(key)
        assignments.append("updated_at = now()")
        return await _with_connection(
            lambda connection: self._update_provider_row(
                connection, key, ", ".join(assignments), args[1:]
            )
        )

    async def _update_provider_row(
        self,
        connection: asyncpg.Connection,
        key: str,
        assignments: str,
        values: list[Any],
    ) -> dict[str, Any] | None:
        row = await connection.fetchrow(
            f"update centaur_meeting_occurrences set {assignments} where occurrence_key = $1 returning *",
            key,
            *values,
        )
        return _serialize_row(row)

    def _zoom_headers(self) -> dict[str, str]:
        # Production uses the brokered ZOOM_ACCESS_TOKEN HTTP secret. The
        # proxy injects the bearer token; the tool never receives Zoom client
        # credentials or exposes them to a caller. A local token may be used
        # for an explicit standalone CLI run.
        token = _secret_value(ZOOM_ACCESS_TOKEN).strip()
        headers = {"Content-Type": "application/json"}
        if token and token != ZOOM_ACCESS_TOKEN:
            headers["Authorization"] = f"Bearer {token}"
        return headers

    def _zoom_request(
        self,
        method: str,
        path: str,
        *,
        payload: dict[str, Any] | None = None,
        occurrence_key: str = "",
    ) -> dict[str, Any]:
        url = f"https://api.zoom.us/v2{path}"
        headers = self._zoom_headers()
        if occurrence_key:
            headers["Idempotency-Key"] = occurrence_key
        with httpx.Client(timeout=30.0) as client:
            response = client.request(method, url, headers=headers, json=payload)
        if response.status_code == 404 and method.upper() == "DELETE":
            return {}
        if response.status_code >= 400:
            failure = f"Zoom request failed with HTTP {response.status_code}"
            detail = _zoom_error_detail(response)
            if detail:
                failure = f"{failure} ({detail})"
            raise MeetingSchedulerError(failure)
        if not response.content:
            return {}
        body = response.json()
        if not isinstance(body, dict):
            raise MeetingSchedulerError("Zoom returned an invalid response")
        return body

    def _zoom_create(
        self,
        *,
        title: str,
        start: dt.datetime,
        duration: int,
        time_zone: str,
        occurrence_key: str,
        organizer_calendar_key: str,
        alternative_host_email: str = "",
    ) -> dict[str, Any]:
        host = _config_value(ZOOM_HOST_USER_ID).strip()
        if not host:
            raise MeetingSchedulerError(f"{ZOOM_HOST_USER_ID} is required")
        payload: dict[str, Any] = {
            "topic": title,
            "type": 2,
            "start_time": _rfc3339(start),
            "duration": duration,
            "timezone": time_zone,
            # Zoom rejects tracking fields that an account administrator has
            # not configured in advance. The agenda is free-form and gives us
            # the same crash-recovery marker without account-global setup.
            "agenda": _zoom_occurrence_marker(occurrence_key),
            "settings": {
                "auto_recording": "cloud",
                "join_before_host": True,
                "jbh_time": 0,
                "meeting_authentication": False,
                "waiting_room": False,
            },
        }
        if alternative_host_email:
            payload["settings"]["alternative_hosts"] = alternative_host_email
        return self._zoom_request(
            "POST",
            "/users/me/meetings",
            occurrence_key=occurrence_key,
            payload=payload,
        )

    @staticmethod
    def _zoom_alternative_hosts(meeting: dict[str, Any]) -> list[str]:
        value = (meeting.get("settings") or {}).get("alternative_hosts") or ""
        return [item.strip().lower() for item in str(value).split(";") if item.strip()]

    def _ensure_zoom_alternative_host(
        self, meeting: dict[str, Any], alternative_host_email: str
    ) -> dict[str, Any]:
        """Make the authenticated proposer a verified Zoom alternative host."""
        alternative_host = alternative_host_email.strip().lower()
        if not alternative_host:
            return meeting
        meeting_id = _require_zoom_meeting_id(meeting.get("id"))
        current = meeting
        if alternative_host not in self._zoom_alternative_hosts(current):
            current = self._zoom_request("GET", f"/meetings/{_zoom_meeting_path_id(meeting_id)}")
        if alternative_host not in self._zoom_alternative_hosts(current):
            self._zoom_request(
                "PATCH",
                f"/meetings/{_zoom_meeting_path_id(meeting_id)}",
                payload={"settings": {"alternative_hosts": alternative_host}},
            )
            current = self._zoom_request("GET", f"/meetings/{_zoom_meeting_path_id(meeting_id)}")
        if alternative_host not in self._zoom_alternative_hosts(current):
            raise MeetingSchedulerError(
                "Zoom did not assign the authenticated proposer as alternative host"
            )
        return current

    def get_recording(self, meeting_id: str) -> dict[str, Any]:
        """Return recording metadata and the bounded VTT transcript, when ready."""

        _require_enabled()
        normalized_id = _require_zoom_meeting_id(meeting_id)
        recording = self._zoom_request(
            "GET", f"/meetings/{_zoom_meeting_path_id(normalized_id)}/recordings"
        )
        meeting_uuid = str(recording.get("uuid") or "").strip()
        meeting_uuid_resolution_error = ""
        if not meeting_uuid and normalized_id.isdigit():
            try:
                instances = self._zoom_request(
                    "GET",
                    f"/past_meetings/{_zoom_meeting_path_id(normalized_id)}/instances",
                )
            except MeetingSchedulerError as error:
                meeting_uuid_resolution_error = str(error)
                instances = {}
            except (httpx.HTTPError, ValueError) as error:
                meeting_uuid_resolution_error = type(error).__name__
                instances = {}
            past_meetings = instances.get("meetings")
            candidates = [
                item
                for item in (past_meetings if isinstance(past_meetings, list) else [])
                if isinstance(item, dict) and str(item.get("uuid") or "").strip()
            ]
            recording_start = str(recording.get("start_time") or "").strip()
            matched = next(
                (
                    item
                    for item in candidates
                    if recording_start
                    and str(item.get("start_time") or "").strip() == recording_start
                ),
                None,
            )
            if matched is None and len(candidates) == 1:
                matched = candidates[0]
            if matched is not None:
                meeting_uuid = str(matched.get("uuid") or "").strip()
        files = recording.get("recording_files")
        public_files: list[dict[str, Any]] = []
        transcript = None
        for item in files if isinstance(files, list) else []:
            if not isinstance(item, dict):
                continue
            public_files.append(
                {
                    key: item.get(key)
                    for key in (
                        "id",
                        "file_type",
                        "file_extension",
                        "file_size",
                        "recording_type",
                        "status",
                    )
                    if item.get(key) is not None
                }
            )
            if transcript is None and str(item.get("file_type") or "").upper() == "TRANSCRIPT":
                download_url = str(item.get("download_url") or "").strip()
                if download_url:
                    transcript = self._zoom_download_transcript(download_url)
        return {
            "meeting_id": recording.get("id") or normalized_id,
            # A numeric meeting ID can be reused across recurring meetings.
            # Zoom's past-meeting endpoints identify the exact completed
            # occurrence by UUID, which may contain '/', '+', and '='.
            "meeting_uuid": meeting_uuid or None,
            "meeting_uuid_resolution_error": meeting_uuid_resolution_error or None,
            "topic": recording.get("topic"),
            "start_time": recording.get("start_time"),
            "recording_files": public_files,
            "transcript": transcript,
            "transcript_status": "ready" if str(transcript or "").strip() else "pending",
        }

    def get_summary(self, meeting_identifier: str) -> dict[str, Any]:
        """Return Zoom AI Companion's processed meeting summary, when available."""

        _require_enabled()
        normalized_identifier = _require_zoom_meeting_id(meeting_identifier)
        summary = self._zoom_request(
            "GET",
            f"/meetings/{_zoom_meeting_path_id(normalized_identifier)}/meeting_summary",
        )
        # Keep the provider payload because Zoom may add summary sections, but
        # never expose token-bearing URLs or provider-internal download links.
        return {
            key: value
            for key, value in summary.items()
            if key not in {"download_url", "play_url", "share_url"}
        }

    def post_meeting_candidates(self, now: str, limit: int = 25) -> list[dict[str, Any]]:
        """Return ended booked meetings whose artifacts have not been delivered."""
        _require_enabled()
        observed_at = _parse_rfc3339(now, field="now")
        bounded_limit = max(1, min(int(limit), 100))

        async def query(connection: asyncpg.Connection) -> list[dict[str, Any]]:
            rows = await connection.fetch(
                """
                select * from centaur_meeting_occurrences
                where status = 'booked'
                  and zoom_meeting_id <> ''
                  and coalesce(actual_start, requested_start)
                      + make_interval(mins => duration_minutes) <= $1
                  and coalesce(metadata->>'post_meeting_status', '') <> 'delivered'
                  and coalesce(metadata->>'post_meeting_status', '') <> 'failed_terminal'
                  and (
                    coalesce(metadata->>'post_meeting_next_retry_at', '') = ''
                    or (metadata->>'post_meeting_next_retry_at')::timestamptz <= $1
                  )
                order by coalesce(actual_start, requested_start), occurrence_key
                limit $2
                """,
                observed_at,
                bounded_limit,
            )
            return [_serialize_row(row) for row in rows]

        return asyncio.run(_with_connection(query))

    def post_meeting_candidate_by_zoom_id(self, meeting_id: str) -> dict[str, Any] | None:
        """Return the exact ended, booked, not-yet-delivered Zoom occurrence."""

        return self._post_meeting_candidate_by_zoom_id(meeting_id, allow_early_end=False)

    def post_meeting_candidate_for_terminal_zoom_event(
        self, meeting_id: str
    ) -> dict[str, Any] | None:
        """Return a candidate selected by an authenticated terminal Zoom event."""

        return self._post_meeting_candidate_by_zoom_id(meeting_id, allow_early_end=True)

    def _post_meeting_candidate_by_zoom_id(
        self, meeting_id: str, *, allow_early_end: bool
    ) -> dict[str, Any] | None:

        _require_enabled()
        normalized_id = _require_zoom_meeting_id(meeting_id)

        async def query(connection: asyncpg.Connection) -> dict[str, Any] | None:
            scheduled_end_clause = (
                ""
                if allow_early_end
                else """
                  and coalesce(actual_start, requested_start)
                      + make_interval(mins => duration_minutes) <= now()
                """
            )
            row = await connection.fetchrow(
                f"""
                select * from centaur_meeting_occurrences
                where status = 'booked'
                  and zoom_meeting_id = $1
                  {scheduled_end_clause}
                  and coalesce(metadata->>'post_meeting_status', '') <> 'delivered'
                order by coalesce(actual_start, requested_start), occurrence_key
                limit 1
                """,
                normalized_id,
            )
            return _serialize_row(row)

        return asyncio.run(_with_connection(query))

    def record_post_meeting_processing(
        self,
        occurrence_key: str,
        *,
        state: str,
        event: str,
        error: str | None = None,
        attempt: int | None = None,
        meeting_id: str | None = None,
        transcript_status: str | None = None,
        summary_source: str | None = None,
        lease_token: str,
    ) -> dict[str, Any]:
        """Record retryable post-meeting processing metadata without delivery."""

        _require_enabled()
        key = _require_occurrence_key(occurrence_key)
        normalized_state = _require_post_meeting_text(
            state, field="state", limit=MAX_POST_MEETING_STATE_LENGTH
        )
        if normalized_state.lower() == "delivered":
            raise MeetingSchedulerError("state cannot mark a meeting delivered")
        normalized_event = _require_post_meeting_text(
            event, field="event", limit=MAX_POST_MEETING_EVENT_LENGTH
        )
        safe_error = str(error or "").strip()[:MAX_POST_MEETING_ERROR_LENGTH]
        if any(ord(char) < 32 for char in safe_error):
            raise MeetingSchedulerError("error must not contain control characters")
        normalized_attempt = _positive_int(attempt, "attempt") if attempt is not None else None
        observed_at = dt.datetime.now(dt.UTC)
        timestamp = _rfc3339(observed_at)
        normalized_meeting_id = (
            _require_zoom_meeting_id(meeting_id) if meeting_id is not None else ""
        )
        normalized_transcript_status = str(transcript_status or "").strip()[:64]
        normalized_summary_source = str(summary_source or "").strip()[:64]
        normalized_lease_token = str(lease_token or "").strip()
        if not normalized_lease_token:
            raise MeetingSchedulerError("lease_token is required")
        if len(normalized_lease_token) > 128 or any(
            char.isspace() for char in normalized_lease_token
        ):
            raise MeetingSchedulerError("lease_token must be a bounded token")

        async def update(connection: asyncpg.Connection) -> dict[str, Any]:
            async with connection.transaction():
                current_row = await connection.fetchrow(
                    "select * from centaur_meeting_occurrences where occurrence_key = $1 for update",
                    key,
                )
                current = _serialize_row(current_row)
                if current is None:
                    raise MeetingSchedulerError("meeting occurrence does not exist")
                if str(current.get("status") or "").lower() != "booked":
                    raise MeetingSchedulerError(
                        "meeting occurrence is no longer eligible for post-meeting processing"
                    )
                metadata = current.get("metadata")
                metadata = metadata if isinstance(metadata, dict) else {}
                if str(metadata.get("post_meeting_status") or "").lower() == "delivered":
                    raise MeetingSchedulerError("meeting occurrence is already delivered")
                if not secrets.compare_digest(
                    str(metadata.get("post_meeting_lease_token") or ""),
                    normalized_lease_token,
                ):
                    raise MeetingSchedulerError("post-meeting processing lease was lost")
                raw_lease_until = str(metadata.get("post_meeting_lease_until") or "").strip()
                try:
                    lease_until = _parse_rfc3339(raw_lease_until, field="post_meeting_lease_until")
                except MeetingSchedulerError as error:
                    raise MeetingSchedulerError(
                        "post-meeting processing lease is invalid"
                    ) from error
                if lease_until <= observed_at:
                    raise MeetingSchedulerError("post-meeting processing lease expired")
                try:
                    previous_attempt = max(0, int(metadata.get("post_meeting_attempt") or 0))
                except (TypeError, ValueError):
                    previous_attempt = 0
                # The atomic claim owns attempt increments. State updates made by
                # the claimed worker preserve that attempt number.
                next_attempt = normalized_attempt or max(previous_attempt, 1)
                retryable = normalized_state.lower() in {
                    "pending_transcript",
                    "error",
                    "failed_retryable",
                }
                terminal = retryable and next_attempt >= 12
                retry_minutes = min(15 * (2 ** max(next_attempt - 1, 0)), 120)
                active_states = {
                    "processing",
                    "summarizing",
                    "publishing_notion",
                    "notifying_participants",
                }
                patch = {
                    "post_meeting_status": "failed_terminal" if terminal else normalized_state,
                    "post_meeting_event": normalized_event,
                    "post_meeting_error": safe_error,
                    "post_meeting_attempt": next_attempt,
                    "post_meeting_attempted_at": timestamp,
                    "post_meeting_updated_at": timestamp,
                    "post_meeting_lease_until": ""
                    if normalized_state not in active_states
                    else _rfc3339(observed_at + dt.timedelta(hours=1)),
                    "post_meeting_lease_token": ""
                    if normalized_state not in active_states
                    else str(metadata.get("post_meeting_lease_token") or ""),
                    "post_meeting_next_retry_at": (
                        _rfc3339(observed_at + dt.timedelta(minutes=retry_minutes))
                        if retryable and not terminal
                        else ""
                    ),
                }
                if normalized_meeting_id:
                    patch["post_meeting_zoom_id"] = normalized_meeting_id
                if normalized_transcript_status:
                    patch["post_meeting_transcript_status"] = normalized_transcript_status
                if normalized_summary_source:
                    patch["post_meeting_summary_source"] = normalized_summary_source
                row = await connection.fetchrow(
                    """
                    update centaur_meeting_occurrences
                    set metadata = metadata || $2::jsonb,
                        version = version + 1, updated_at = now()
                    where occurrence_key = $1
                    returning *
                    """,
                    key,
                    json.dumps(patch),
                )
                if row is None:
                    raise MeetingSchedulerError("meeting occurrence does not exist")
                return _serialize_row(row) or {}

        return asyncio.run(_with_connection(update))

    def claim_post_meeting_processing(
        self,
        occurrence_key: str,
        *,
        event: str,
        lease_seconds: int = 600,
        force: bool = False,
        owner_token: str | None = None,
        meeting_uuid: str | None = None,
    ) -> dict[str, Any]:
        """Atomically lease one occurrence so concurrent Zoom events cannot duplicate work."""

        _require_enabled()
        key = _require_occurrence_key(occurrence_key)
        normalized_event = _require_post_meeting_text(
            event, field="event", limit=MAX_POST_MEETING_EVENT_LENGTH
        )
        bounded_lease_seconds = max(60, min(int(lease_seconds), 3600))
        normalized_owner_token = str(owner_token or "").strip()
        if normalized_owner_token and not re.fullmatch(r"[0-9a-f]{64}", normalized_owner_token):
            raise MeetingSchedulerError("owner_token must be a SHA-256 token")
        normalized_meeting_uuid = (
            _require_zoom_meeting_id(meeting_uuid) if meeting_uuid is not None else ""
        )
        now = dt.datetime.now(dt.UTC)
        lease_until = now + dt.timedelta(seconds=bounded_lease_seconds)

        async def claim(connection: asyncpg.Connection) -> dict[str, Any]:
            async with connection.transaction():
                current_row = await connection.fetchrow(
                    "select * from centaur_meeting_occurrences where occurrence_key = $1 for update",
                    key,
                )
                current = _serialize_row(current_row)
                if current is None:
                    raise MeetingSchedulerError("meeting occurrence does not exist")
                if str(current.get("status") or "").lower() != "booked":
                    return {"claimed": False, "reason": "not_booked"}
                metadata = current.get("metadata")
                metadata = metadata if isinstance(metadata, dict) else {}
                # The authenticated webhook carries Zoom's exact completed-
                # occurrence UUID. Persist it before lease arbitration so a
                # racing scheduler worker and every later retry can use it.
                if (
                    normalized_meeting_uuid
                    and str(metadata.get("post_meeting_zoom_uuid") or "") != normalized_meeting_uuid
                ):
                    uuid_patch = {"post_meeting_zoom_uuid": normalized_meeting_uuid}
                    await connection.fetchrow(
                        """
                        update centaur_meeting_occurrences
                        set metadata = metadata || $2::jsonb,
                            version = version + 1, updated_at = now()
                        where occurrence_key = $1
                        returning *
                        """,
                        key,
                        json.dumps(uuid_patch),
                    )
                    metadata = {**metadata, **uuid_patch}
                status = str(metadata.get("post_meeting_status") or "").lower()
                if status == "delivered" or str(current.get("status") or "").lower() == "completed":
                    return {"claimed": False, "reason": "already_delivered"}
                if status == "failed_terminal" and not force:
                    return {"claimed": False, "reason": "failed_terminal"}
                raw_next_retry = str(metadata.get("post_meeting_next_retry_at") or "").strip()
                retry_override_events = {
                    "recording.transcript_completed",
                    "meeting.summary_completed",
                }
                if (
                    status in {"pending_transcript", "failed_retryable", "error"}
                    and raw_next_retry
                    and not force
                    and not (
                        status == "pending_transcript" and normalized_event in retry_override_events
                    )
                ):
                    try:
                        next_retry = _parse_rfc3339(
                            raw_next_retry, field="post_meeting_next_retry_at"
                        )
                    except MeetingSchedulerError:
                        next_retry = now
                    if next_retry > now:
                        return {"claimed": False, "reason": "retry_not_due"}
                raw_lease_until = str(metadata.get("post_meeting_lease_until") or "").strip()
                if raw_lease_until:
                    try:
                        active_until = _parse_rfc3339(
                            raw_lease_until, field="post_meeting_lease_until"
                        )
                    except MeetingSchedulerError:
                        active_until = now
                    if (
                        status
                        in {
                            "processing",
                            "summarizing",
                            "publishing_notion",
                            "notifying_participants",
                        }
                        and active_until > now
                    ):
                        active_token = str(metadata.get("post_meeting_lease_token") or "")
                        if normalized_owner_token and secrets.compare_digest(
                            active_token, normalized_owner_token
                        ):
                            patch = {
                                "post_meeting_updated_at": _rfc3339(now),
                                "post_meeting_lease_until": _rfc3339(lease_until),
                            }
                            resumed_row = await connection.fetchrow(
                                """
                                update centaur_meeting_occurrences
                                set metadata = metadata || $2::jsonb,
                                    version = version + 1, updated_at = now()
                                where occurrence_key = $1
                                returning *
                                """,
                                key,
                                json.dumps(patch),
                            )
                            return {
                                "claimed": True,
                                "occurrence_key": key,
                                "occurrence": _serialize_row(resumed_row),
                                "lease_token": active_token,
                                "lease_until": patch["post_meeting_lease_until"],
                                "attempt": max(
                                    1,
                                    int(metadata.get("post_meeting_attempt") or 1),
                                ),
                                "resumed": True,
                            }
                        return {"claimed": False, "reason": "active_lease"}
                try:
                    previous_attempt = max(0, int(metadata.get("post_meeting_attempt") or 0))
                except (TypeError, ValueError):
                    previous_attempt = 0
                lease_token = normalized_owner_token or secrets.token_hex(32)
                patch = {
                    "post_meeting_status": "processing",
                    "post_meeting_event": normalized_event,
                    "post_meeting_attempt": previous_attempt + 1,
                    "post_meeting_attempted_at": _rfc3339(now),
                    "post_meeting_updated_at": _rfc3339(now),
                    "post_meeting_lease_until": _rfc3339(lease_until),
                    "post_meeting_lease_token": lease_token,
                }
                row = await connection.fetchrow(
                    """
                    update centaur_meeting_occurrences
                    set metadata = metadata || $2::jsonb,
                        version = version + 1, updated_at = now()
                    where occurrence_key = $1
                    returning *
                    """,
                    key,
                    json.dumps(patch),
                )
                if row is None:
                    raise MeetingSchedulerError("meeting occurrence does not exist")
                return {
                    "claimed": True,
                    "lease_token": lease_token,
                    "occurrence": _serialize_row(row),
                }

        return asyncio.run(_with_connection(claim))

    def collect_post_meeting_artifacts(self, meeting_id: str) -> dict[str, Any]:
        """Collect processed Zoom artifacts without failing while Zoom is still processing."""
        recording: dict[str, Any] = {"transcript_status": "pending"}
        summary: dict[str, Any] = {}
        errors: list[str] = []
        try:
            recording = self.get_recording(meeting_id)
        except MeetingSchedulerError as error:
            errors.append(str(error))
        uuid_resolution_error = str(recording.get("meeting_uuid_resolution_error") or "").strip()
        if uuid_resolution_error:
            errors.append(f"Zoom meeting UUID resolution failed: {uuid_resolution_error}")
        try:
            summary_identifier = str(recording.get("meeting_uuid") or meeting_id).strip()
            summary = self.get_summary(summary_identifier)
        except MeetingSchedulerError as error:
            errors.append(str(error))
        summary_text = str(summary.get("meeting_summary") or summary.get("summary") or "").strip()
        raw_action_items = summary.get("next_steps") or summary.get("action_items") or ""
        action_items = (
            "\n".join(str(item) for item in raw_action_items)
            if isinstance(raw_action_items, list)
            else str(raw_action_items or "").strip()
        )
        return {
            "meeting_id": str(meeting_id),
            "ready": recording.get("transcript_status") == "ready",
            "transcript": recording.get("transcript"),
            "transcript_status": recording.get("transcript_status", "pending"),
            "recording_files": recording.get("recording_files", []),
            "action_items": action_items,
            "summary": summary,
            "summary_text": summary_text,
            "summary_source": "zoom" if summary_text else "unavailable",
            "processing_errors": errors,
        }

    def mark_post_meeting_delivered(
        self,
        occurrence_key: str,
        *,
        notion_page_id: str | None = None,
        delivered_to: list[str] | None = None,
        lease_token: str,
    ) -> dict[str, Any]:
        """Persist the terminal idempotency marker after publication and delivery."""
        key = _require_occurrence_key(occurrence_key)
        normalized_lease_token = str(lease_token or "").strip()
        if not normalized_lease_token:
            raise MeetingSchedulerError("lease_token is required")
        if len(normalized_lease_token) > 128 or any(
            char.isspace() for char in normalized_lease_token
        ):
            raise MeetingSchedulerError("lease_token must be a bounded token")
        patch = {
            "post_meeting_status": "delivered",
            "post_meeting_notion_page_id": str(notion_page_id or ""),
            "post_meeting_delivered_to": sorted(set(delivered_to or [])),
            "post_meeting_delivered_at": dt.datetime.now(dt.UTC).isoformat(),
            "post_meeting_lease_until": "",
            "post_meeting_lease_token": "",
        }

        async def update(connection: asyncpg.Connection) -> dict[str, Any]:
            row = await connection.fetchrow(
                """
                update centaur_meeting_occurrences
                set status = 'completed', metadata = metadata || $2::jsonb,
                    last_error = '', version = version + 1, updated_at = now()
                where occurrence_key = $1
                  and status = 'booked'
                  and metadata->>'post_meeting_lease_token' = $3
                  and nullif(metadata->>'post_meeting_lease_until', '')::timestamptz > now()
                returning *
                """,
                key,
                json.dumps(patch),
                normalized_lease_token,
            )
            if row is None:
                raise MeetingSchedulerError(
                    "meeting occurrence does not exist or processing lease was lost"
                )
            return _serialize_row(row)

        return asyncio.run(_with_connection(update))

    def _zoom_download_transcript(self, download_url: str) -> str:
        return asyncio.run(self._zoom_download_transcript_async(download_url))

    async def _zoom_download_transcript_async(self, download_url: str) -> str:
        current_url = download_url
        try:
            async with asyncio.timeout(ZOOM_TRANSCRIPT_DOWNLOAD_TIMEOUT_SECONDS):
                async with httpx.AsyncClient(timeout=30.0, follow_redirects=False) as client:
                    for redirect_count in range(6):
                        parsed = urlparse(current_url)
                        hostname = (parsed.hostname or "").lower()
                        if parsed.scheme != "https" or not (
                            hostname == "zoom.us" or hostname.endswith(".zoom.us")
                        ):
                            raise MeetingSchedulerError(
                                "Zoom returned an invalid transcript download URL"
                            )
                        # Zoom's first response authorizes the download and redirects
                        # to a signed URL. Never forward the bearer token beyond that
                        # initial request, even to another Zoom-owned hostname.
                        headers = self._zoom_headers() if redirect_count == 0 else {}
                        async with client.stream("GET", current_url, headers=headers) as response:
                            if response.status_code in {301, 302, 303, 307, 308}:
                                location = response.headers.get("location", "").strip()
                                if not location:
                                    raise MeetingSchedulerError(
                                        "Zoom transcript download redirect had no location"
                                    )
                                if redirect_count == 5:
                                    raise MeetingSchedulerError(
                                        "Zoom transcript download exceeded the redirect limit"
                                    )
                                current_url = urljoin(current_url, location)
                                continue
                            if response.status_code >= 300:
                                raise MeetingSchedulerError(
                                    "Zoom transcript download failed with HTTP "
                                    f"{response.status_code}"
                                )
                            content = bytearray()
                            async for chunk in response.aiter_bytes():
                                if len(content) + len(chunk) > MAX_TRANSCRIPT_BYTES:
                                    raise MeetingSchedulerError(
                                        "Zoom transcript exceeded the size limit"
                                    )
                                content.extend(chunk)
                            return bytes(content).decode("utf-8-sig")
        except TimeoutError as exc:
            raise MeetingSchedulerError("Zoom transcript download exceeded the time limit") from exc
        raise MeetingSchedulerError("Zoom transcript download exceeded the redirect limit")

    @staticmethod
    def _calendar_event_id(key: str) -> str:
        """Return a deterministic Google Calendar event ID for retry reuse."""

        digest = hashlib.sha256(key.encode("utf-8")).hexdigest()
        # Google Calendar event IDs are restricted to lowercase letters a-v
        # and digits. Keep the stable identity while avoiding separators such
        # as ``_`` that the Calendar API rejects.
        return f"centaur{digest[:40]}"

    def _calendar_event_body(
        self,
        *,
        key: str,
        title: str,
        start: dt.datetime,
        duration: int,
        time_zone: str,
        attendees: list[str],
        join_url: str,
    ) -> dict[str, Any]:
        end = start + dt.timedelta(minutes=duration)
        return {
            "summary": title,
            "description": f"Zoom: {join_url}",
            "location": join_url,
            "start": {"dateTime": _rfc3339_in_zone(start, time_zone), "timeZone": time_zone},
            "end": {"dateTime": _rfc3339_in_zone(end, time_zone), "timeZone": time_zone},
            "attendees": [{"email": email} for email in attendees],
            "guestsCanModify": True,
            "guestsCanInviteOthers": True,
            "extendedProperties": {"private": {"centaurOccurrenceKey": key}},
        }

    def _calendar_find_by_occurrence(self, calendar_id: str, key: str) -> dict[str, Any] | None:
        try:
            response = (
                get_calendar_service()
                .events()
                .list(
                    calendarId=calendar_id,
                    privateExtendedProperty=f"centaurOccurrenceKey={key}",
                    showDeleted=False,
                    maxResults=1,
                )
                .execute()
            )
        except Exception:
            return None
        items = response.get("items") if isinstance(response, dict) else None
        return (
            items[0] if isinstance(items, list) and items and isinstance(items[0], dict) else None
        )

    def _zoom_find_by_occurrence(self, key: str) -> dict[str, Any] | None:
        if not _config_value(ZOOM_HOST_USER_ID).strip():
            return None
        next_page_token = ""
        # The occurrence key is the retry identity. Search all bounded pages so
        # an older occurrence cannot be missed and recreated after a partial
        # failure when the host has more than one page of scheduled meetings.
        for _ in range(20):
            query = "/users/me/meetings?type=scheduled&page_size=300"
            if next_page_token:
                query += f"&next_page_token={quote(next_page_token, safe='')}"
            try:
                response = self._zoom_request("GET", query)
            except Exception:
                return None
            meetings = response.get("meetings") if isinstance(response, dict) else None
            for meeting in meetings if isinstance(meetings, list) else []:
                if not isinstance(meeting, dict):
                    continue
                if meeting.get("agenda") == _zoom_occurrence_marker(key):
                    return meeting
                # Retain discovery compatibility for meetings created before
                # the agenda marker replaced the account-configured field.
                for tracking in meeting.get("tracking_fields") or []:
                    if isinstance(tracking, dict) and tracking.get("value") == key:
                        return meeting
            next_page_token = str(response.get("next_page_token") or "").strip()
            if not next_page_token:
                break
        return None

    def book_meeting(
        self,
        occurrence_key: str,
        title: str,
        start: str,
        duration_minutes: int,
        time_zone: str,
        attendee_emails: list[str],
        organizer_calendar_key: str,
        cadence_id: str | None = None,
        request_id: str | None = None,
        mode: str = "cadence",
        confirmation_token: str | None = None,
        alternative_host_email: str | None = None,
        visibility: str = "public",
    ) -> dict[str, Any]:
        """Create or reuse one Zoom + Calendar meeting occurrence."""
        _require_enabled()
        key = _require_occurrence_key(occurrence_key)
        if mode not in {"cadence", "ad_hoc"}:
            raise MeetingSchedulerError("mode must be cadence or ad_hoc")
        if mode == "ad_hoc" and not str(confirmation_token or "").strip():
            raise MeetingSchedulerError("ad-hoc booking requires explicit confirmation")
        attendees = _email_list(attendee_emails)
        visibility = str(visibility or "").strip().lower()
        if visibility not in {"public", "private"}:
            raise MeetingSchedulerError("visibility must be public or private")
        alternative_host = ""
        if alternative_host_email:
            alternative_host = _email_list([alternative_host_email])[0]
            if mode != "ad_hoc":
                raise MeetingSchedulerError(
                    "alternative hosts are only supported for confirmed ad-hoc meetings"
                )
            if alternative_host not in attendees:
                raise MeetingSchedulerError("alternative host must be a meeting attendee")
        start_at = _parse_rfc3339(start, field="start")
        duration = _positive_int(duration_minutes, "duration_minutes")
        organizer_id, _ = self._calendar_ids(organizer_calendar_key, attendees)
        if start_at <= dt.datetime.now(dt.UTC):
            raise MeetingSchedulerError("meeting start must be in the future")
        if mode == "ad_hoc":
            expected_confirmation = _slot_confirmation_token(
                start=start_at,
                duration=duration,
                time_zone=time_zone,
                attendees=attendees,
                organizer_calendar_key=organizer_calendar_key,
                visibility=visibility,
            )
            legacy_public_confirmation = (
                _slot_confirmation_token(
                    start=start_at,
                    duration=duration,
                    time_zone=time_zone,
                    attendees=attendees,
                    organizer_calendar_key=organizer_calendar_key,
                )
                if visibility == "public"
                else ""
            )
            if not (
                _matches_confirmation(confirmation_token, expected_confirmation)
                or (
                    legacy_public_confirmation
                    and _matches_confirmation(
                        confirmation_token, legacy_public_confirmation
                    )
                )
            ):
                raise MeetingSchedulerError(
                    "confirmation does not match the requested meeting slot"
                )
            self._assert_slot_free(
                organizer_id=organizer_id,
                attendee_emails=attendees,
                start=start_at,
                duration=duration,
            )
        result = asyncio.run(
            self._book_meeting_locked(
                key=key,
                cadence_id=cadence_id,
                request_id=request_id,
                title=title,
                start_at=start_at,
                duration=duration,
                time_zone=time_zone,
                organizer_calendar_key=organizer_calendar_key,
                organizer_id=organizer_id,
                attendees=attendees,
                alternative_host_email=alternative_host,
                visibility=visibility,
                allow_parameter_update=mode == "cadence",
                check_slot_free=mode == "ad_hoc",
            )
        )
        if isinstance(result, _OperationFailure):
            error = result.error
            if isinstance(error, MeetingSchedulerError):
                raise error
            raise MeetingSchedulerError("meeting provider booking failed") from error
        return result

    async def _book_meeting_locked(
        self,
        *,
        key: str,
        cadence_id: str | None,
        request_id: str | None,
        title: str,
        start_at: dt.datetime,
        duration: int,
        time_zone: str,
        organizer_calendar_key: str,
        organizer_id: str,
        attendees: list[str],
        allow_parameter_update: bool,
        check_slot_free: bool,
        alternative_host_email: str = "",
        visibility: str = "public",
    ) -> dict[str, Any] | _OperationFailure:
        async def operation(connection: asyncpg.Connection) -> dict[str, Any] | _OperationFailure:
            organizer_date = start_at.astimezone(_zone(time_zone)).date().isoformat()
            await connection.execute(
                "select pg_advisory_xact_lock(hashtextextended($1, 0))",
                f"organizer-day:{organizer_id}:{organizer_date}",
            )
            state, inserted = await self._claim_occurrence_row(
                connection,
                key=key,
                cadence_id=cadence_id,
                request_id=request_id,
                title=title,
                requested_start=start_at,
                duration=duration,
                time_zone=time_zone,
                organizer_key=organizer_calendar_key,
                organizer_id=organizer_id,
                attendees=attendees,
                allow_parameter_update=allow_parameter_update,
                visibility=visibility,
            )
            if not inserted:
                for field, value in (
                    ("cadence_id", cadence_id),
                    ("request_id", request_id),
                ):
                    stored = str(state.get(field) or "").strip()
                    requested = str(value or "").strip()
                    if stored and requested and stored != requested:
                        raise MeetingSchedulerError(
                            "occurrence identity does not match existing state"
                        )
                if str(state.get("organizer_calendar_key") or "") != organizer_calendar_key:
                    raise MeetingSchedulerError("occurrence identity does not match existing state")
            if state.get("status") in {"completed", "cancelled"}:
                return {"status": state["status"], **state}
            if state.get("status") == "booked":
                if not allow_parameter_update:
                    return {
                        "status": "booked",
                        **state,
                        "actualStart": _rfc3339(
                            _parse_rfc3339(
                                str(
                                    state.get("actual_start") or state.get("requested_start") or ""
                                ),
                                field="actual_start",
                            )
                        ),
                        "zoomJoinUrl": str(state.get("zoom_join_url") or ""),
                    }
                return await self._reconcile_booked_locked(
                    connection,
                    state=state,
                    key=key,
                    title=title,
                    start_at=start_at,
                    duration=duration,
                    time_zone=time_zone,
                    organizer_id=organizer_id,
                    attendees=attendees,
                )

            # Free/busy is only a snapshot. Recheck while holding the same
            # organizer/day advisory lock that serializes competing bookings.
            # A partial retry with an existing Calendar event is already bound
            # to this occurrence and must not reject itself as busy.
            if check_slot_free and not state.get("calendar_event_id"):
                self._assert_slot_free(
                    organizer_id=organizer_id,
                    attendee_emails=attendees,
                    start=start_at,
                    duration=duration,
                )

            try:
                zoom_id = str(state.get("zoom_meeting_id") or "").strip()
                join_url = str(state.get("zoom_join_url") or "").strip()
                if zoom_id:
                    try:
                        existing_zoom = self._zoom_request(
                            "GET", f"/meetings/{quote(zoom_id, safe='')}"
                        )
                    except Exception:
                        # A stale provider ID must not be treated as a valid
                        # meeting. Reconcile by occurrence or create a new
                        # Zoom meeting under the same stable key below.
                        zoom_id = ""
                        join_url = ""
                    else:
                        join_url = str(existing_zoom.get("join_url") or join_url).strip()
                        self._ensure_zoom_alternative_host(existing_zoom, alternative_host_email)
                if not zoom_id:
                    existing_zoom = self._zoom_find_by_occurrence(key)
                    if existing_zoom:
                        zoom = existing_zoom
                    else:
                        zoom = self._zoom_create(
                            title=title,
                            start=start_at,
                            duration=duration,
                            time_zone=time_zone,
                            occurrence_key=key,
                            organizer_calendar_key=organizer_calendar_key,
                            alternative_host_email=alternative_host_email,
                        )
                    zoom = self._ensure_zoom_alternative_host(zoom, alternative_host_email)
                    join_url = str(zoom.get("join_url") or "").strip()
                    zoom_id = str(zoom.get("id") or "").strip()
                if not join_url or not zoom_id:
                    raise MeetingSchedulerError("Zoom returned no meeting ID or join URL")
                if not state.get("zoom_meeting_id"):
                    await self._update_provider_row(
                        connection,
                        key,
                        "zoom_meeting_id = $2, zoom_join_url = $3, updated_at = now()",
                        [zoom_id, join_url],
                    )

                service = get_calendar_service()
                event_id = str(state.get("calendar_event_id") or "").strip()
                event: dict[str, Any] = {}
                if event_id:
                    try:
                        event = (
                            service.events()
                            .get(calendarId=organizer_id, eventId=event_id)
                            .execute()
                        )
                    except Exception:
                        event_id = ""
                if not event_id:
                    event_id = self._calendar_event_id(key)
                    body = self._calendar_event_body(
                        key=key,
                        title=title,
                        start=start_at,
                        duration=duration,
                        time_zone=time_zone,
                        attendees=attendees,
                        join_url=join_url,
                    )
                    body["id"] = event_id
                    try:
                        event = (
                            service.events()
                            .insert(
                                calendarId=organizer_id,
                                body=body,
                                sendUpdates="all",
                            )
                            .execute()
                        )
                    except Exception as insert_error:
                        try:
                            event = (
                                service.events()
                                .get(calendarId=organizer_id, eventId=event_id)
                                .execute()
                            )
                        except Exception as get_error:
                            raise insert_error from get_error
                if not event_id:
                    raise MeetingSchedulerError("Calendar returned no event ID")
                if not state.get("calendar_event_id"):
                    await self._update_provider_row(
                        connection,
                        key,
                        "calendar_event_id = $2, calendar_html_link = $3, updated_at = now()",
                        [event_id, str(event.get("htmlLink") or "")],
                    )
                result = await self._mark_booked_row(
                    connection,
                    key,
                    {
                        "actual_start": start_at,
                        "organizer_id": organizer_id,
                        "event_id": event_id,
                        "event_link": str(event.get("htmlLink") or ""),
                        "zoom_id": zoom_id,
                        "join_url": join_url,
                    },
                )
                return {
                    "status": "booked",
                    **(result or {}),
                    "actualStart": _rfc3339(start_at),
                    "zoomJoinUrl": join_url,
                }
            except Exception as error:
                await connection.execute(
                    "update centaur_meeting_occurrences set status = 'blocked', last_error = $2, updated_at = now() where occurrence_key = $1",
                    key,
                    str(error)[:2000],
                )
                return _OperationFailure(error)

        return await _with_occurrence_lock(key, operation)

    async def _reconcile_booked_locked(
        self,
        connection: asyncpg.Connection,
        *,
        state: dict[str, Any],
        key: str,
        title: str,
        start_at: dt.datetime,
        duration: int,
        time_zone: str,
        organizer_id: str,
        attendees: list[str],
    ) -> dict[str, Any] | _OperationFailure:
        """Reconcile an upcoming booked occurrence without changing its key."""

        actual_start_value = state.get("actual_start") or state.get("requested_start")
        requested_start_value = state.get("requested_start")
        if not actual_start_value or not requested_start_value:
            raise MeetingSchedulerError("booked meeting has no occurrence start")
        actual_start = _parse_rfc3339(str(actual_start_value), field="actual_start")
        requested_start = _parse_rfc3339(str(requested_start_value), field="requested_start")
        if actual_start <= dt.datetime.now(dt.UTC):
            raise MeetingSchedulerError("started meetings cannot be automatically changed")

        stored_attendees = [
            str(item).strip().lower()
            for item in (state.get("attendee_emails") or [])
            if str(item).strip()
        ]
        # Additions can affect the current event. Removals intentionally do
        # not: the next stable occurrence will use the new cadence attendee
        # set, while the already-booked occurrence remains safely inclusive.
        effective_attendees = list(dict.fromkeys([*stored_attendees, *attendees]))
        old_title = str(state.get("title") or "")
        old_duration = int(state.get("duration_minutes") or 0)
        title_changed = old_title != title
        duration_changed = old_duration != duration
        # ``requested_start`` is the cadence anchor for this stable
        # occurrence. A manual reschedule changes only ``actual_start`` and
        # must survive the next cadence reconciliation. A changed cadence
        # start, by contrast, updates the same provider pair and then records
        # the new requested anchor for future retries.
        cadence_start_changed = requested_start != start_at
        desired_start = start_at if cadence_start_changed else actual_start
        start_changed = desired_start != actual_start
        attendees_changed = effective_attendees != stored_attendees
        if not (title_changed or duration_changed or start_changed or attendees_changed):
            return {
                "status": "booked",
                **state,
                "actualStart": _rfc3339(actual_start),
                "zoomJoinUrl": str(state.get("zoom_join_url") or ""),
            }

        zoom_id = str(state.get("zoom_meeting_id") or "").strip()
        join_url = str(state.get("zoom_join_url") or "").strip()
        event_id = str(state.get("calendar_event_id") or "").strip()
        if not zoom_id or not join_url:
            raise MeetingSchedulerError("booked meeting has incomplete Zoom provider state")
        if not event_id:
            raise MeetingSchedulerError("booked meeting has no Calendar event ID")

        service = None
        original_event: dict[str, Any] = {}
        zoom_update_attempted = False
        calendar_update_attempted = False
        zoom_changed = False
        try:
            service = get_calendar_service()
            existing_event = (
                service.events().get(calendarId=organizer_id, eventId=event_id).execute()
            )
            original_event = dict(existing_event)
            zoom_payload: dict[str, Any] = {}
            if title_changed:
                zoom_payload["topic"] = title
            if start_changed:
                zoom_payload["start_time"] = _rfc3339(start_at)
                zoom_payload["timezone"] = time_zone
            if duration_changed:
                zoom_payload["duration"] = duration
            if zoom_payload:
                zoom_update_attempted = True
                self._zoom_request(
                    "PATCH",
                    f"/meetings/{zoom_id}",
                    occurrence_key=key,
                    payload=zoom_payload,
                )
                zoom_changed = True

            event = self._calendar_event_body(
                key=key,
                title=title,
                start=desired_start,
                duration=duration,
                time_zone=time_zone,
                attendees=effective_attendees,
                join_url=join_url,
            )
            calendar_update_attempted = True
            updated = (
                service.events()
                .update(
                    calendarId=organizer_id,
                    eventId=event_id,
                    body=event,
                    sendUpdates="all",
                )
                .execute()
            )
            result = await self._reconcile_booked_row(
                connection,
                key=key,
                title=title,
                requested_start=start_at if cadence_start_changed else requested_start,
                actual_start=desired_start,
                duration=duration,
                attendees=effective_attendees,
            )
            return {
                "status": "booked",
                **(result or state),
                "actualStart": _rfc3339(desired_start),
                "zoomJoinUrl": join_url,
                "calendarHtmlLink": str(
                    updated.get("htmlLink") or state.get("calendar_html_link") or ""
                ),
                "reconciled": True,
            }
        except Exception as error:
            compensation_errors: list[str] = []
            if calendar_update_attempted and service is not None:
                try:
                    service.events().update(
                        calendarId=organizer_id,
                        eventId=event_id,
                        body=original_event,
                        sendUpdates="all",
                    ).execute()
                except Exception as compensation_error:
                    compensation_errors.append(f"Calendar rollback failed: {compensation_error}")
            if zoom_update_attempted or zoom_changed:
                try:
                    rollback_payload: dict[str, Any] = {}
                    if title_changed:
                        rollback_payload["topic"] = old_title
                    if start_changed:
                        rollback_payload["start_time"] = _rfc3339(actual_start)
                        rollback_payload["timezone"] = str(state["time_zone"])
                    if duration_changed:
                        rollback_payload["duration"] = old_duration
                    if rollback_payload:
                        self._zoom_request(
                            "PATCH",
                            f"/meetings/{zoom_id}",
                            occurrence_key=key,
                            payload=rollback_payload,
                        )
                except Exception as compensation_error:
                    compensation_errors.append(f"Zoom rollback failed: {compensation_error}")
            if compensation_errors:
                await connection.execute(
                    "update centaur_meeting_occurrences set status = 'blocked', last_error = $2, updated_at = now() where occurrence_key = $1",
                    key,
                    (str(error) + "; " + "; ".join(compensation_errors))[:2000],
                )
            else:
                await connection.execute(
                    "update centaur_meeting_occurrences set last_error = $2, updated_at = now() where occurrence_key = $1",
                    key,
                    str(error)[:2000],
                )
            return _OperationFailure(error)

    async def _get_existing(self, key: str) -> dict[str, Any] | None:
        return await _with_connection(lambda connection: self._get_occurrence(connection, key))

    async def _mark_error(self, key: str, message: str) -> None:
        await _with_connection(
            lambda connection: connection.execute(
                "update centaur_meeting_occurrences set status = 'blocked', last_error = $2, updated_at = now() where occurrence_key = $1",
                key,
                message[:2000],
            )
        )

    async def _mark_booked(self, key: str, **values: Any) -> dict[str, Any] | None:
        return await _with_connection(
            lambda connection: self._mark_booked_row(connection, key, values)
        )

    async def _mark_booked_row(
        self, connection: asyncpg.Connection, key: str, values: dict[str, Any]
    ) -> dict[str, Any] | None:
        row = await connection.fetchrow(
            """
            update centaur_meeting_occurrences
            set status = 'booked', actual_start = $2, organizer_calendar_id = $3,
                calendar_event_id = $4, calendar_html_link = $5, zoom_meeting_id = $6,
                zoom_join_url = $7, version = version + 1, last_error = '', updated_at = now()
            where occurrence_key = $1
            returning *
            """,
            key,
            values["actual_start"],
            values["organizer_id"],
            values["event_id"],
            values["event_link"],
            values["zoom_id"],
            values["join_url"],
        )
        return _serialize_row(row)

    async def _reconcile_booked_row(
        self,
        connection: asyncpg.Connection,
        *,
        key: str,
        title: str,
        requested_start: dt.datetime,
        actual_start: dt.datetime,
        duration: int,
        attendees: list[str],
    ) -> dict[str, Any] | None:
        row = await connection.fetchrow(
            """
            update centaur_meeting_occurrences
            set title = $2, requested_start = $3, actual_start = $4,
                duration_minutes = $5, attendee_emails = $6,
                version = version + 1, last_error = '', updated_at = now()
            where occurrence_key = $1
            returning *
            """,
            key,
            title,
            requested_start,
            actual_start,
            duration,
            attendees,
        )
        return _serialize_row(row)

    async def _reconcile_actual_start(
        self,
        connection: asyncpg.Connection,
        key: str,
        start: dt.datetime,
        expected_version: int,
    ) -> dict[str, Any] | None:
        row = await connection.fetchrow(
            """
            update centaur_meeting_occurrences
            set actual_start = $2, version = version + 1, updated_at = now()
            where occurrence_key = $1 and version = $3
            returning *
            """,
            key,
            start,
            expected_version,
        )
        return _serialize_row(row)

    def reschedule_meeting(
        self,
        occurrence_key: str,
        start: str,
        expected_version: int,
        organizer_calendar_key: str,
        mode: str = "cadence",
        confirmation_token: str | None = None,
    ) -> dict[str, Any]:
        """Move the existing provider pair before the occurrence starts."""
        _require_enabled()
        key = _require_occurrence_key(occurrence_key)
        if mode not in {"cadence", "ad_hoc"}:
            raise MeetingSchedulerError("mode must be cadence or ad_hoc")
        if mode == "ad_hoc" and not str(confirmation_token or "").strip():
            raise MeetingSchedulerError("ad-hoc rescheduling requires explicit confirmation")
        new_start = _parse_rfc3339(start, field="start")
        result = asyncio.run(
            self._reschedule_meeting_locked(
                key=key,
                new_start=new_start,
                expected_version=int(expected_version),
                organizer_calendar_key=organizer_calendar_key,
                confirmation_token=confirmation_token if mode == "ad_hoc" else None,
            )
        )
        if isinstance(result, _OperationFailure):
            error = result.error
            if isinstance(error, MeetingSchedulerError):
                raise error
            raise MeetingSchedulerError("meeting provider rescheduling failed") from error
        return result

    async def _reschedule_meeting_locked(
        self,
        *,
        key: str,
        new_start: dt.datetime,
        expected_version: int,
        organizer_calendar_key: str,
        confirmation_token: str | None,
    ) -> dict[str, Any] | _OperationFailure:
        async def operation(connection: asyncpg.Connection) -> dict[str, Any] | _OperationFailure:
            state = await self._get_occurrence(connection, key)
            if not state:
                raise MeetingSchedulerError("meeting occurrence does not exist")
            if int(state.get("version") or 0) != expected_version:
                raise MeetingSchedulerError("meeting occurrence version is stale")
            if state.get("status") != "booked":
                raise MeetingSchedulerError("only booked meetings can be rescheduled")
            if str(state.get("organizer_calendar_key") or "") != organizer_calendar_key:
                raise MeetingSchedulerError("organizer calendar does not match occurrence state")
            if new_start <= dt.datetime.now(dt.UTC):
                raise MeetingSchedulerError("started meetings cannot be automatically rescheduled")
            duration = int(state["duration_minutes"])
            attendees = _email_list(state.get("attendee_emails") or [])
            if confirmation_token is not None:
                expected_confirmation = _slot_confirmation_token(
                    start=new_start,
                    duration=duration,
                    time_zone=str(state["time_zone"]),
                    attendees=attendees,
                    organizer_calendar_key=organizer_calendar_key,
                )
                if not _matches_confirmation(confirmation_token, expected_confirmation):
                    raise MeetingSchedulerError(
                        "confirmation does not match the requested meeting slot"
                    )
            current_start = _parse_rfc3339(
                str(state.get("actual_start") or state.get("requested_start") or ""),
                field="actual_start",
            )
            if new_start != current_start:
                await connection.execute(
                    "select pg_advisory_xact_lock(hashtextextended($1, 0))",
                    f"organizer-day:{state['organizer_calendar_id']}:{new_start.astimezone(_zone(str(state['time_zone']))).date().isoformat()}",
                )
                self._assert_slot_free(
                    organizer_id=str(state["organizer_calendar_id"]),
                    attendee_emails=attendees,
                    start=new_start,
                    duration=duration,
                )
            zoom_id = str(state.get("zoom_meeting_id") or "")
            if not zoom_id:
                raise MeetingSchedulerError("booked meeting has no Zoom meeting ID")
            event_id = str(state.get("calendar_event_id") or "")
            if not event_id:
                raise MeetingSchedulerError("booked meeting has no Calendar event ID")

            service = None
            original_event: dict[str, Any] = {}
            zoom_update_attempted = False
            calendar_update_attempted = False
            zoom_changed = False
            try:
                service = get_calendar_service()
                event = (
                    service.events()
                    .get(calendarId=state["organizer_calendar_id"], eventId=event_id)
                    .execute()
                )
                original_event = dict(event)
                zoom_update_attempted = True
                self._zoom_request(
                    "PATCH",
                    f"/meetings/{zoom_id}",
                    occurrence_key=key,
                    payload={
                        "start_time": _rfc3339(new_start),
                        "duration": duration,
                        "timezone": state["time_zone"],
                    },
                )
                zoom_changed = True
                event["start"] = {
                    "dateTime": _rfc3339_in_zone(new_start, state["time_zone"]),
                    "timeZone": state["time_zone"],
                }
                event["end"] = {
                    "dateTime": _rfc3339_in_zone(
                        new_start + dt.timedelta(minutes=duration), state["time_zone"]
                    ),
                    "timeZone": state["time_zone"],
                }
                calendar_update_attempted = True
                updated = (
                    service.events()
                    .update(
                        calendarId=state["organizer_calendar_id"],
                        eventId=event_id,
                        body=event,
                        sendUpdates="all",
                    )
                    .execute()
                )
                result = await self._reschedule_row(connection, key, new_start, expected_version)
                return {
                    "status": "booked",
                    **(result or {}),
                    "actualStart": _rfc3339(new_start),
                    "calendarHtmlLink": updated.get("htmlLink", ""),
                }
            except Exception as error:
                compensation_errors: list[str] = []
                if calendar_update_attempted and service is not None:
                    try:
                        service.events().update(
                            calendarId=state["organizer_calendar_id"],
                            eventId=event_id,
                            body=original_event,
                            sendUpdates="all",
                        ).execute()
                    except Exception as compensation_error:
                        compensation_errors.append(
                            f"Calendar rollback failed: {compensation_error}"
                        )
                if zoom_update_attempted or zoom_changed:
                    try:
                        self._zoom_request(
                            "PATCH",
                            f"/meetings/{zoom_id}",
                            occurrence_key=key,
                            payload={
                                "start_time": _rfc3339(current_start),
                                "duration": duration,
                                "timezone": state["time_zone"],
                            },
                        )
                    except Exception as compensation_error:
                        compensation_errors.append(f"Zoom rollback failed: {compensation_error}")
                if compensation_errors:
                    await connection.execute(
                        "update centaur_meeting_occurrences set status = 'blocked', last_error = $2, updated_at = now() where occurrence_key = $1",
                        key,
                        (str(error) + "; " + "; ".join(compensation_errors))[:2000],
                    )
                else:
                    await connection.execute(
                        "update centaur_meeting_occurrences set last_error = $2, updated_at = now() where occurrence_key = $1",
                        key,
                        str(error)[:2000],
                    )
                return _OperationFailure(error)

        return await _with_occurrence_lock(key, operation)

    async def _mark_rescheduled(
        self, key: str, start: dt.datetime, version: int
    ) -> dict[str, Any] | None:
        return await _with_connection(
            lambda connection: self._reschedule_row(connection, key, start, version)
        )

    async def _reschedule_row(
        self, connection: asyncpg.Connection, key: str, start: dt.datetime, version: int
    ) -> dict[str, Any] | None:
        row = await connection.fetchrow(
            "update centaur_meeting_occurrences set actual_start = $2, version = version + 1, updated_at = now() where occurrence_key = $1 and version = $3 returning *",
            key,
            start,
            version,
        )
        if row is None:
            raise MeetingSchedulerError("meeting occurrence changed while rescheduling")
        return _serialize_row(row)

    def cancel_meeting(
        self,
        occurrence_key: str,
        organizer_calendar_key: str,
        confirmation_token: str | None = None,
    ) -> dict[str, Any]:
        _require_enabled()
        key = _require_occurrence_key(occurrence_key)
        if not str(confirmation_token or "").strip():
            raise MeetingSchedulerError("cancellation requires explicit confirmation")
        expected_confirmation = _cancel_confirmation_token(
            occurrence_key=key,
            organizer_calendar_key=organizer_calendar_key,
        )
        if not _matches_confirmation(confirmation_token, expected_confirmation):
            raise MeetingSchedulerError("confirmation does not match the requested meeting")
        result = asyncio.run(
            self._cancel_meeting_locked(key=key, organizer_calendar_key=organizer_calendar_key)
        )
        if isinstance(result, _OperationFailure):
            error = result.error
            if isinstance(error, MeetingSchedulerError):
                raise error
            raise MeetingSchedulerError("meeting provider cancellation failed") from error
        return result

    async def _cancel_meeting_locked(
        self, *, key: str, organizer_calendar_key: str
    ) -> dict[str, Any] | _OperationFailure:
        async def operation(connection: asyncpg.Connection) -> dict[str, Any] | _OperationFailure:
            state = _serialize_row(
                await connection.fetchrow(
                    """
                    select * from centaur_meeting_occurrences
                    where occurrence_key = $1
                    for update
                    """,
                    key,
                )
            )
            if not state:
                return {"status": "not_found", "occurrenceKey": key}
            if str(state.get("organizer_calendar_key") or "") != organizer_calendar_key:
                raise MeetingSchedulerError("organizer calendar does not match occurrence state")
            if state.get("status") == "cancelled":
                return {
                    "status": "cancelled",
                    "occurrenceKey": key,
                    "cadence_id": state.get("cadence_id"),
                }
            if state.get("status") != "booked":
                return {
                    "status": state.get("status"),
                    "occurrenceKey": key,
                    "cadence_id": state.get("cadence_id"),
                }
            try:
                if state.get("zoom_meeting_id"):
                    self._zoom_request(
                        "DELETE",
                        f"/meetings/{state['zoom_meeting_id']}",
                        occurrence_key=key,
                    )
                if state.get("calendar_event_id"):
                    try:
                        get_calendar_service().events().delete(
                            calendarId=state["organizer_calendar_id"],
                            eventId=state["calendar_event_id"],
                            sendUpdates="all",
                        ).execute()
                    except Exception as error:
                        if getattr(getattr(error, "resp", None), "status", None) != 404:
                            raise
                await connection.execute(
                    """
                    update centaur_meeting_occurrences
                    set status = 'cancelled',
                        metadata = metadata || $2::jsonb,
                        version = version + 1,
                        last_error = '',
                        updated_at = now()
                    where occurrence_key = $1
                    """,
                    key,
                    json.dumps(
                        {
                            "post_meeting_status": "cancelled",
                            "post_meeting_lease_until": "",
                            "post_meeting_lease_token": "",
                        }
                    ),
                )
                return {
                    "status": "cancelled",
                    "occurrenceKey": key,
                    "cadence_id": state.get("cadence_id"),
                }
            except Exception as error:
                await connection.execute(
                    "update centaur_meeting_occurrences set last_error = $2, updated_at = now() where occurrence_key = $1",
                    key,
                    str(error)[:2000],
                )
                return _OperationFailure(error)

        return await _with_occurrence_lock(key, operation)

    def end_meeting(
        self,
        occurrence_key: str,
        organizer_calendar_key: str,
        confirmation_token: str | None = None,
    ) -> dict[str, Any]:
        """End an managed live Zoom meeting without deleting its event."""

        _require_enabled()
        key = _require_occurrence_key(occurrence_key)
        if not str(confirmation_token or "").strip():
            raise MeetingSchedulerError("ending a meeting requires explicit confirmation")
        expected_confirmation = _end_confirmation_token(
            occurrence_key=key,
            organizer_calendar_key=organizer_calendar_key,
        )
        if not _matches_confirmation(confirmation_token, expected_confirmation):
            raise MeetingSchedulerError("confirmation does not match the requested meeting")
        result = asyncio.run(
            self._end_meeting_locked(key=key, organizer_calendar_key=organizer_calendar_key)
        )
        if isinstance(result, _OperationFailure):
            error = result.error
            if isinstance(error, MeetingSchedulerError):
                raise error
            raise MeetingSchedulerError("Zoom meeting end failed") from error
        return result

    async def _end_meeting_locked(
        self, *, key: str, organizer_calendar_key: str
    ) -> dict[str, Any] | _OperationFailure:
        async def operation(connection: asyncpg.Connection) -> dict[str, Any] | _OperationFailure:
            state = _serialize_row(
                await connection.fetchrow(
                    """
                    select * from centaur_meeting_occurrences
                    where occurrence_key = $1
                    for update
                    """,
                    key,
                )
            )
            if not state:
                return {"status": "not_found", "occurrenceKey": key}
            if str(state.get("organizer_calendar_key") or "") != organizer_calendar_key:
                raise MeetingSchedulerError("organizer calendar does not match occurrence state")
            if state.get("status") != "booked":
                return {
                    "status": state.get("status"),
                    "occurrenceKey": key,
                    "cadence_id": state.get("cadence_id"),
                }
            meeting_id = str(state.get("zoom_meeting_id") or "").strip()
            if not meeting_id:
                raise MeetingSchedulerError("booked meeting has no Zoom meeting ID")
            try:
                self._zoom_request(
                    "PUT",
                    f"/meetings/{meeting_id}/status",
                    payload={"action": "end"},
                    occurrence_key=key,
                )
                await connection.execute(
                    """
                    update centaur_meeting_occurrences
                    set metadata = metadata || jsonb_build_object(
                            'zoom_ended_by_centaur_at', now()::text
                        ),
                        last_error = '',
                        updated_at = now()
                    where occurrence_key = $1
                    """,
                    key,
                )
                return {
                    "status": "ended",
                    "occurrenceKey": key,
                    "cadence_id": state.get("cadence_id"),
                }
            except Exception as error:
                await connection.execute(
                    "update centaur_meeting_occurrences set last_error = $2, updated_at = now() where occurrence_key = $1",
                    key,
                    str(error)[:2000],
                )
                return _OperationFailure(error)

        return await _with_occurrence_lock(key, operation)

    def get_or_reconcile_meeting(self, occurrence_key: str) -> dict[str, Any]:
        _require_enabled()
        key = _require_occurrence_key(occurrence_key)

        async def reconcile(connection: asyncpg.Connection) -> dict[str, Any]:
            # The advisory lock is held for the complete reconciliation. Keep
            # the provider-state read behind the client seam so recovery can be
            # tested without a live asyncpg connection.
            state = await self._get_existing(key)
            if not state:
                return {"status": "not_found", "occurrenceKey": key}
            if not state.get("calendar_event_id"):
                event = self._calendar_find_by_occurrence(state["organizer_calendar_id"], key)
                if event and event.get("id"):
                    state["calendar_event_id"] = str(event["id"])
                    state["calendar_html_link"] = str(event.get("htmlLink") or "")
                    event_start = (event.get("start") or {}).get("dateTime")
                    if event_start:
                        state["actual_start"] = str(event_start)
                    await self._update_provider_state(
                        key,
                        event_id=state["calendar_event_id"],
                        event_link=state["calendar_html_link"],
                    )
            if state.get("zoom_meeting_id") and not state.get("zoom_join_url"):
                try:
                    meeting = self._zoom_request("GET", f"/meetings/{state['zoom_meeting_id']}")
                    join_url = str(meeting.get("join_url") or "").strip()
                    if join_url:
                        state["zoom_join_url"] = join_url
                        await self._update_provider_state(key, join_url=join_url)
                except Exception:
                    pass
            if not state.get("zoom_meeting_id"):
                meeting = self._zoom_find_by_occurrence(key)
                if meeting and meeting.get("id") and meeting.get("join_url"):
                    state["zoom_meeting_id"] = str(meeting["id"])
                    state["zoom_join_url"] = str(meeting["join_url"])
                    await self._update_provider_state(
                        key,
                        zoom_id=state["zoom_meeting_id"],
                        join_url=state["zoom_join_url"],
                    )
            provider_state = {"calendarPresent": None, "zoomPresent": None}
            calendar_start: dt.datetime | None = None
            zoom_start: dt.datetime | None = None
            if state.get("calendar_event_id"):
                try:
                    calendar_event = (
                        get_calendar_service()
                        .events()
                        .get(
                            calendarId=state["organizer_calendar_id"],
                            eventId=state["calendar_event_id"],
                        )
                        .execute()
                    )
                    provider_state["calendarPresent"] = True
                    calendar_start_value = (calendar_event.get("start") or {}).get("dateTime")
                    if calendar_start_value:
                        calendar_start = _parse_rfc3339(
                            str(calendar_start_value), field="calendar.start"
                        )
                except Exception:
                    provider_state["calendarPresent"] = False
            if state.get("zoom_meeting_id"):
                try:
                    zoom_meeting = self._zoom_request(
                        "GET", f"/meetings/{state['zoom_meeting_id']}"
                    )
                    provider_state["zoomPresent"] = bool(state.get("zoom_join_url"))
                    zoom_start_value = zoom_meeting.get("start_time")
                    if zoom_start_value:
                        zoom_start = _parse_rfc3339(str(zoom_start_value), field="zoom.start")
                except Exception:
                    provider_state["zoomPresent"] = False
            provider_mismatch = (
                calendar_start is not None
                and zoom_start is not None
                and calendar_start != zoom_start
            )
            if provider_mismatch:
                await connection.execute(
                    "update centaur_meeting_occurrences set status = 'blocked', last_error = $2, updated_at = now() where occurrence_key = $1",
                    key,
                    "Calendar and Zoom provider times disagree; reconciliation is required",
                )
                state["status"] = "blocked"
            provider_start = calendar_start or zoom_start
            if (
                not provider_mismatch
                and state.get("status") == "booked"
                and provider_start is not None
                and provider_start
                != _parse_rfc3339(
                    str(state.get("actual_start") or state.get("requested_start") or ""),
                    field="actual_start",
                )
            ):
                reconciled = await self._reconcile_actual_start(
                    connection,
                    key,
                    provider_start,
                    int(state.get("version") or 0),
                )
                if reconciled:
                    state = reconciled
            if (
                state.get("status") in {"pending", "blocked"}
                and not provider_mismatch
                and provider_state["calendarPresent"] is True
                and provider_state["zoomPresent"] is True
            ):
                reconciled_start = (
                    provider_start or state.get("actual_start") or state["requested_start"]
                )
                if isinstance(reconciled_start, str):
                    reconciled_start = _parse_rfc3339(reconciled_start, field="actual_start")
                state = (
                    await self._mark_booked(
                        key,
                        actual_start=reconciled_start,
                        organizer_id=state["organizer_calendar_id"],
                        event_id=state["calendar_event_id"],
                        event_link=state.get("calendar_html_link") or "",
                        zoom_id=state["zoom_meeting_id"],
                        join_url=state["zoom_join_url"],
                    )
                    or state
                )
            elif state.get("status") == "booked" and (
                provider_state["calendarPresent"] is not True
                or provider_state["zoomPresent"] is not True
            ):
                # A previously booked row with a missing provider pair must be
                # retryable. Otherwise the cadence broker would see ``booked``
                # and the next book attempt could incorrectly return stale
                # provider IDs without repairing Calendar or Zoom.
                await self._mark_error(
                    key,
                    "provider state is incomplete and requires reconciliation",
                )
                state["status"] = "blocked"
            result = {
                "status": str(state.get("status") or "pending"),
                **state,
                "providerState": provider_state,
            }
            result["cancelConfirmationToken"] = _cancel_confirmation_token(
                occurrence_key=key,
                organizer_calendar_key=str(state.get("organizer_calendar_key") or ""),
            )
            result["endConfirmationToken"] = _end_confirmation_token(
                occurrence_key=key,
                organizer_calendar_key=str(state.get("organizer_calendar_key") or ""),
            )
            return result

        return asyncio.run(_with_occurrence_lock(key, reconcile))


def _client() -> MeetingSchedulerClient:
    return MeetingSchedulerClient()


def find_availability(
    organizer_calendar_key: str,
    attendee_emails: list[str],
    time_min: str,
    time_max: str,
    duration_minutes: int,
    response_timezone: str = DEFAULT_TIME_ZONE,
    working_start: str = "09:00",
    working_end: str = "17:00",
) -> dict[str, Any]:
    return _client().find_availability(
        organizer_calendar_key,
        attendee_emails,
        time_min,
        time_max,
        duration_minutes,
        response_timezone,
        working_start,
        working_end,
    )


def book_meeting(
    occurrence_key: str,
    title: str,
    start: str,
    duration_minutes: int,
    time_zone: str,
    attendee_emails: list[str],
    organizer_calendar_key: str,
    cadence_id: str | None = None,
    request_id: str | None = None,
    mode: str = "cadence",
    confirmation_token: str | None = None,
    alternative_host_email: str | None = None,
    visibility: str = "public",
) -> dict[str, Any]:
    return _client().book_meeting(
        occurrence_key,
        title,
        start,
        duration_minutes,
        time_zone,
        attendee_emails,
        organizer_calendar_key,
        cadence_id,
        request_id,
        mode,
        confirmation_token,
        alternative_host_email=alternative_host_email,
        visibility=visibility,
    )


def reschedule_meeting(
    occurrence_key: str,
    start: str,
    expected_version: int,
    organizer_calendar_key: str,
    mode: str = "cadence",
    confirmation_token: str | None = None,
) -> dict[str, Any]:
    return _client().reschedule_meeting(
        occurrence_key,
        start,
        expected_version,
        organizer_calendar_key,
        mode,
        confirmation_token,
    )


def cancel_meeting(
    occurrence_key: str,
    organizer_calendar_key: str,
    confirmation_token: str | None = None,
) -> dict[str, Any]:
    return _client().cancel_meeting(occurrence_key, organizer_calendar_key, confirmation_token)


def end_meeting(
    occurrence_key: str,
    organizer_calendar_key: str,
    confirmation_token: str | None = None,
) -> dict[str, Any]:
    return _client().end_meeting(occurrence_key, organizer_calendar_key, confirmation_token)


def get_or_reconcile_meeting(occurrence_key: str) -> dict[str, Any]:
    return _client().get_or_reconcile_meeting(occurrence_key)
