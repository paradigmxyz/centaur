from __future__ import annotations

from typing import Any

from api.integrations.gsuite.http import build_http


def get_calendar_service():
    """Return a proxy-authenticated Google Calendar v3 service."""
    from googleapiclient.discovery import build

    return build("calendar", "v3", http=build_http())


class GoogleCalendarReadonlyClient:
    """Read-only Calendar client used by ETL workflows."""

    def list_calendars(self, *, page_token: str | None = None) -> dict[str, Any]:
        service = get_calendar_service()
        kwargs: dict[str, Any] = {
            "showHidden": True,
            "fields": (
                "nextPageToken, items("
                "id, summary, description, location, timeZone, accessRole, primary, "
                "selected, hidden, backgroundColor, foregroundColor"
                ")"
            ),
        }
        if page_token:
            kwargs["pageToken"] = page_token
        return service.calendarList().list(**kwargs).execute()

    def list_events(
        self,
        *,
        calendar_id: str,
        page_size: int,
        page_token: str | None = None,
        sync_token: str | None = None,
    ) -> dict[str, Any]:
        service = get_calendar_service()
        kwargs: dict[str, Any] = {
            "calendarId": calendar_id,
            "maxResults": page_size,
            "showDeleted": True,
            "fields": (
                "nextPageToken, nextSyncToken, items("
                "id, iCalUID, status, summary, description, location, htmlLink, "
                "created, updated, start, end, creator, organizer, attendees, "
                "recurringEventId, originalStartTime, transparency, visibility, "
                "eventType, sequence, hangoutLink, conferenceData"
                ")"
            ),
        }
        if page_token:
            kwargs["pageToken"] = page_token
        if sync_token:
            kwargs["syncToken"] = sync_token
        return service.events().list(**kwargs).execute()
