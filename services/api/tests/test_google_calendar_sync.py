from __future__ import annotations

import datetime as dt
import importlib
import json
from typing import Any

import pytest
import pytest_asyncio


class FakeCtx:
    def __init__(self, db_pool, run_id: str = "wfr-test-google-calendar-sync"):
        self._pool = db_pool
        self.run_id = run_id
        self.logs: list[tuple[str, dict[str, Any]]] = []

    def log(self, msg: str, **kwargs: Any) -> None:
        self.logs.append((msg, kwargs))


class _GoneResponse:
    status = 410


class SyncTokenGone(Exception):
    resp = _GoneResponse()


class FakeCalendarClient:
    def __init__(
        self,
        *,
        calendar_pages: list[dict[str, Any]],
        event_pages: dict[str, list[dict[str, Any] | Exception]],
    ) -> None:
        self.calendar_pages = list(calendar_pages)
        self.event_pages = {key: list(value) for key, value in event_pages.items()}
        self.calendar_calls: list[dict[str, Any]] = []
        self.event_calls: list[dict[str, Any]] = []

    def list_calendars(self, *, page_token: str | None = None) -> dict[str, Any]:
        self.calendar_calls.append({"page_token": page_token})
        if self.calendar_pages:
            return self.calendar_pages.pop(0)
        return {"items": []}

    def list_events(
        self,
        *,
        calendar_id: str,
        page_size: int,
        page_token: str | None = None,
        sync_token: str | None = None,
    ) -> dict[str, Any]:
        self.event_calls.append(
            {
                "calendar_id": calendar_id,
                "page_size": page_size,
                "page_token": page_token,
                "sync_token": sync_token,
            }
        )
        page = self.event_pages[calendar_id].pop(0)
        if isinstance(page, Exception):
            raise page
        return page


@pytest_asyncio.fixture(autouse=True)
async def _clear_google_calendar_tables(db_pool):
    await db_pool.execute(
        "TRUNCATE TABLE google_calendar_sync_checkpoints, google_calendar_sync_events, "
        "google_calendar_sync_calendars, google_calendar_sync_runs, workflow_runs CASCADE",
    )
    yield


def test_schedule_defaults_disabled_with_four_hour_interval(monkeypatch):
    monkeypatch.delenv("GOOGLE_CALENDAR_ETL_ENABLED", raising=False)
    monkeypatch.delenv("GOOGLE_CALENDAR_SYNC_INTERVAL_SECONDS", raising=False)

    from workflows import google_calendar_sync

    reloaded = importlib.reload(google_calendar_sync)

    assert reloaded.SCHEDULE == {
        "schedule_id": "google_calendar_sync",
        "interval_seconds": 14400,
        "enabled": False,
        "no_delivery": True,
    }


def test_schedule_respects_env_overrides(monkeypatch):
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")
    monkeypatch.setenv("GOOGLE_CALENDAR_SYNC_INTERVAL_SECONDS", "300")

    from workflows import google_calendar_sync

    reloaded = importlib.reload(google_calendar_sync)

    assert reloaded.SCHEDULE["enabled"] is True
    assert reloaded.SCHEDULE["interval_seconds"] == 300


@pytest.mark.asyncio
async def test_syncs_calendar_events_into_raw_tables(db_pool, monkeypatch):
    from workflows import google_calendar_sync

    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")
    fake = FakeCalendarClient(
        calendar_pages=[
            {
                "items": [
                    {
                        "id": "primary@example.com",
                        "summary": "Primary",
                        "timeZone": "America/Los_Angeles",
                        "accessRole": "owner",
                        "primary": True,
                    }
                ]
            }
        ],
        event_pages={
            "primary@example.com": [
                {
                    "items": [
                        {
                            "id": "event-1",
                            "iCalUID": "ical-1@example.com",
                            "status": "confirmed",
                            "summary": "Roadmap review",
                            "description": "Discuss launch plan",
                            "location": "Room 1",
                            "htmlLink": "https://calendar.google.com/event?eid=event-1",
                            "created": "2026-05-01T10:00:00Z",
                            "updated": "2026-05-02T10:00:00Z",
                            "start": {"dateTime": "2026-05-03T17:00:00Z"},
                            "end": {"dateTime": "2026-05-03T18:00:00Z"},
                            "creator": {"email": "alice@example.com"},
                            "organizer": {"email": "bob@example.com"},
                            "attendees": [
                                {"email": "carol@example.com", "displayName": "Carol"}
                            ],
                            "visibility": "default",
                            "eventType": "default",
                            "sequence": 3,
                        }
                    ],
                    "nextSyncToken": "sync-token-1",
                }
            ]
        },
    )
    monkeypatch.setattr(google_calendar_sync, "_client", lambda: fake)

    result = await google_calendar_sync.handler(
        google_calendar_sync.Input(limit=25),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["calendars_seen"] == 1
    assert result["events_upserted"] == 1
    assert fake.event_calls == [
        {
            "calendar_id": "primary@example.com",
            "page_size": 25,
            "page_token": None,
            "sync_token": None,
        }
    ]

    calendar = await db_pool.fetchrow(
        "SELECT calendar_id, summary, time_zone, access_role, is_primary "
        "FROM google_calendar_sync_calendars",
    )
    assert calendar["calendar_id"] == "primary@example.com"
    assert calendar["summary"] == "Primary"
    assert calendar["time_zone"] == "America/Los_Angeles"
    assert calendar["access_role"] == "owner"
    assert calendar["is_primary"] is True

    event = await db_pool.fetchrow(
        "SELECT calendar_id, event_id, summary, description, location, attendees, "
        "start_at, end_at, source_updated_at, content_text, status "
        "FROM google_calendar_sync_events",
    )
    assert event["calendar_id"] == "primary@example.com"
    assert event["event_id"] == "event-1"
    assert event["summary"] == "Roadmap review"
    assert event["description"] == "Discuss launch plan"
    assert event["location"] == "Room 1"
    assert json.loads(event["attendees"])[0]["email"] == "carol@example.com"
    assert event["start_at"] == dt.datetime(2026, 5, 3, 17, 0, tzinfo=dt.timezone.utc)
    assert event["end_at"] == dt.datetime(2026, 5, 3, 18, 0, tzinfo=dt.timezone.utc)
    assert event["source_updated_at"] == dt.datetime(
        2026, 5, 2, 10, 0, tzinfo=dt.timezone.utc
    )
    assert "Roadmap review" in event["content_text"]
    assert "Carol" in event["content_text"]
    assert event["status"] == "confirmed"

    checkpoint = await db_pool.fetchrow(
        "SELECT calendar_id, sync_token, watermark_time FROM google_calendar_sync_checkpoints",
    )
    assert checkpoint["calendar_id"] == "primary@example.com"
    assert checkpoint["sync_token"] == "sync-token-1"
    assert checkpoint["watermark_time"] == dt.datetime(
        2026, 5, 2, 10, 0, tzinfo=dt.timezone.utc
    )


@pytest.mark.asyncio
async def test_incremental_sync_uses_checkpoint_token(db_pool, monkeypatch):
    from workflows import google_calendar_sync

    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_runs (run_id, status) VALUES ('seed', 'completed')"
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_calendars (calendar_id, summary) "
        "VALUES ('primary@example.com', 'Primary')",
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_checkpoints (calendar_id, sync_token, watermark_time) "
        "VALUES ('primary@example.com', 'old-token', $1)",
        dt.datetime(2026, 5, 1, tzinfo=dt.timezone.utc),
    )
    fake = FakeCalendarClient(
        calendar_pages=[{"items": [{"id": "primary@example.com", "summary": "Primary"}]}],
        event_pages={
            "primary@example.com": [
                {
                    "items": [
                        {
                            "id": "event-2",
                            "status": "cancelled",
                            "updated": "2026-05-03T10:00:00Z",
                        }
                    ],
                    "nextSyncToken": "new-token",
                }
            ]
        },
    )
    monkeypatch.setattr(google_calendar_sync, "_client", lambda: fake)

    result = await google_calendar_sync.handler(
        google_calendar_sync.Input(),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["events_cancelled"] == 1
    assert fake.event_calls[0]["sync_token"] == "old-token"
    checkpoint = await db_pool.fetchrow(
        "SELECT sync_token FROM google_calendar_sync_checkpoints "
        "WHERE calendar_id = 'primary@example.com'",
    )
    assert checkpoint["sync_token"] == "new-token"


@pytest.mark.asyncio
async def test_expired_sync_token_retries_full_sync(db_pool, monkeypatch):
    from workflows import google_calendar_sync

    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_calendars (calendar_id, summary) "
        "VALUES ('primary@example.com', 'Primary')",
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_checkpoints (calendar_id, sync_token) "
        "VALUES ('primary@example.com', 'expired-token')",
    )
    fake = FakeCalendarClient(
        calendar_pages=[{"items": [{"id": "primary@example.com", "summary": "Primary"}]}],
        event_pages={
            "primary@example.com": [
                SyncTokenGone("410 Gone"),
                {"items": [], "nextSyncToken": "replacement-token"},
            ]
        },
    )
    monkeypatch.setattr(google_calendar_sync, "_client", lambda: fake)
    ctx = FakeCtx(db_pool)

    result = await google_calendar_sync.handler(
        google_calendar_sync.Input(),
        ctx,
    )

    assert result["status"] == "completed"
    assert fake.event_calls[0]["sync_token"] == "expired-token"
    assert fake.event_calls[1]["sync_token"] is None
    checkpoint = await db_pool.fetchrow(
        "SELECT sync_token FROM google_calendar_sync_checkpoints "
        "WHERE calendar_id = 'primary@example.com'",
    )
    assert checkpoint["sync_token"] == "replacement-token"
    assert any(log[0] == "google_calendar_sync_token_expired" for log in ctx.logs)
