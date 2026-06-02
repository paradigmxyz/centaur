from __future__ import annotations

import datetime as dt
import importlib
import json
from typing import Any

import pytest
import pytest_asyncio


class FakeCtx:
    def __init__(self, db_pool, run_id: str = "wfr-test-google-drive-sync"):
        self._pool = db_pool
        self.run_id = run_id
        self.logs: list[tuple[str, dict[str, Any]]] = []

    def log(self, msg: str, **kwargs: Any) -> None:
        self.logs.append((msg, kwargs))


class FakeDriveClient:
    def __init__(self, pages: list[dict[str, Any]], texts: dict[str, str]):
        self.pages = list(pages)
        self.texts = texts
        self.list_calls: list[dict[str, Any]] = []
        self.text_calls: list[str] = []

    def list_docs(
        self,
        *,
        query: str,
        page_size: int,
        page_token: str | None = None,
    ) -> dict[str, Any]:
        self.list_calls.append(
            {"query": query, "page_size": page_size, "page_token": page_token}
        )
        if self.pages:
            return self.pages.pop(0)
        return {"files": []}

    def docs_get_text(self, document_id: str) -> str:
        self.text_calls.append(document_id)
        value = self.texts[document_id]
        if isinstance(value, Exception):
            raise value
        return value


@pytest_asyncio.fixture(autouse=True)
async def _clear_google_drive_tables(db_pool):
    await db_pool.execute(
        "TRUNCATE TABLE company_context_documents, google_drive_sync_checkpoints, "
        "google_drive_sync_files, google_drive_sync_runs, google_calendar_sync_checkpoints, "
        "google_calendar_sync_events, google_calendar_sync_calendars, google_calendar_sync_runs, "
        "workflow_runs CASCADE",
    )
    yield


def test_schedule_defaults_disabled_with_four_hour_interval(monkeypatch):
    monkeypatch.delenv("GOOGLE_DRIVE_ETL_ENABLED", raising=False)
    monkeypatch.delenv("GOOGLE_DRIVE_SYNC_INTERVAL_SECONDS", raising=False)

    from workflows import google_drive_sync

    reloaded = importlib.reload(google_drive_sync)

    assert reloaded.SCHEDULE == {
        "schedule_id": "google_drive_sync",
        "interval_seconds": 14400,
        "enabled": False,
        "no_delivery": True,
    }


def test_schedule_respects_env_overrides(monkeypatch):
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "true")
    monkeypatch.setenv("GOOGLE_DRIVE_SYNC_INTERVAL_SECONDS", "300")

    from workflows import google_drive_sync

    reloaded = importlib.reload(google_drive_sync)

    assert reloaded.SCHEDULE["enabled"] is True
    assert reloaded.SCHEDULE["interval_seconds"] == 300


@pytest.mark.asyncio
async def test_syncs_google_docs_into_raw_tables(db_pool, monkeypatch):
    from workflows import google_drive_sync

    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "true")
    fake = FakeDriveClient(
        pages=[
            {
                "files": [
                    {
                        "id": "doc-1",
                        "name": "Investment memo",
                        "mimeType": "application/vnd.google-apps.document",
                        "webViewLink": "https://docs.google.com/document/d/doc-1/edit",
                        "driveId": "shared-drive-1",
                        "parents": ["folder-1"],
                        "owners": [
                            {
                                "emailAddress": "owner@example.com",
                                "displayName": "Owner Example",
                            }
                        ],
                        "lastModifyingUser": {
                            "emailAddress": "editor@example.com",
                            "displayName": "Editor Example",
                        },
                        "createdTime": "2026-05-01T12:00:00Z",
                        "modifiedTime": "2026-05-02T12:00:00Z",
                    }
                ],
            }
        ],
        texts={"doc-1": "Doc body\nWith details"},
    )
    monkeypatch.setattr(google_drive_sync, "_client", lambda: fake)

    result = await google_drive_sync.handler(
        google_drive_sync.Input(limit=25, watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["files_seen"] == 1
    assert result["docs_upserted"] == 1
    assert fake.list_calls[0]["page_size"] == 25
    assert " in parents" not in fake.list_calls[0]["query"]
    assert (
        "mimeType = 'application/vnd.google-apps.document'"
        in fake.list_calls[0]["query"]
    )

    row = await db_pool.fetchrow(
        "SELECT file_id, name, text_content, web_view_link, parent_ids, owners, "
        "source_modified_at, last_error FROM google_drive_sync_files",
    )
    assert row["file_id"] == "doc-1"
    assert row["name"] == "Investment memo"
    assert row["text_content"] == "Doc body\nWith details"
    assert row["web_view_link"] == "https://docs.google.com/document/d/doc-1/edit"
    assert json.loads(row["parent_ids"]) == ["folder-1"]
    assert json.loads(row["owners"])[0]["emailAddress"] == "owner@example.com"
    assert row["source_modified_at"] == dt.datetime(
        2026, 5, 2, 12, 0, tzinfo=dt.timezone.utc
    )
    assert row["last_error"] == ""

    checkpoint = await db_pool.fetchrow(
        "SELECT scope_id, watermark_time FROM google_drive_sync_checkpoints",
    )
    assert checkpoint["scope_id"] == "all_visible"
    assert checkpoint["watermark_time"] == dt.datetime(
        2026, 5, 2, 12, 0, tzinfo=dt.timezone.utc
    )


@pytest.mark.asyncio
async def test_syncs_all_visible_docs_without_scope_config(db_pool, monkeypatch):
    from workflows import google_drive_sync

    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "true")
    fake = FakeDriveClient(pages=[{"files": []}], texts={})
    monkeypatch.setattr(google_drive_sync, "_client", lambda: fake)

    result = await google_drive_sync.handler(
        google_drive_sync.Input(),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert (
        "mimeType = 'application/vnd.google-apps.document'"
        in fake.list_calls[0]["query"]
    )
    assert " in parents" not in fake.list_calls[0]["query"]


@pytest.mark.asyncio
async def test_incremental_query_uses_checkpoint_overlap(db_pool, monkeypatch):
    from workflows import google_drive_sync

    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "true")
    await db_pool.execute(
        "INSERT INTO google_drive_sync_checkpoints (scope_id, watermark_time) "
        "VALUES ('all_visible', $1)",
        dt.datetime(2026, 5, 10, 12, 0, tzinfo=dt.timezone.utc),
    )
    fake = FakeDriveClient(pages=[{"files": []}], texts={})
    monkeypatch.setattr(google_drive_sync, "_client", lambda: fake)

    result = await google_drive_sync.handler(
        google_drive_sync.Input(watermark_overlap_seconds=60),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert "modifiedTime > '2026-05-10T11:59:00Z'" in fake.list_calls[0]["query"]
