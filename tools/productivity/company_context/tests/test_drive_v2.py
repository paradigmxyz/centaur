from __future__ import annotations

import datetime as dt
import sys
from pathlib import Path

import asyncpg
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from company_context.drive_v2 import DriveV2Client


class _FakeConnection:
    def __init__(self, *, results=None) -> None:
        self.results = list(results or [])
        self.queries = []
        self.closed = False

    async def _next(self, query, args, default):
        self.queries.append((query, args))
        result = self.results.pop(0) if self.results else default
        if isinstance(result, BaseException):
            raise result
        return result

    async def fetch(self, query, *args):
        return await self._next(query, args, [])

    async def fetchrow(self, query, *args):
        return await self._next(query, args, None)

    async def fetchval(self, query, *args):
        return await self._next(query, args, 1)

    async def close(self):
        self.closed = True


@pytest.fixture
def connect(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "false")

    def install(fake: _FakeConnection) -> _FakeConnection:
        async def fake_connect(*args, **kwargs):
            return fake

        monkeypatch.setattr(asyncpg, "connect", fake_connect)
        return fake

    return install


def _drive_row(**overrides):
    row = {
        "document_id": "google-drive:file-1:0",
        "file_id": "file-1",
        "chunk_id": "0",
        "document_type": "pdf",
        "title": "Roadmap.pdf",
        "body": "Roadmap PDF covers launch sequencing.",
        "url": "https://drive.google.com/file/d/file-1/view",
        "mime_type": "application/pdf",
        "drive_id": "shared-drive-1",
        "page_start": 1,
        "page_end": 2,
        "source_created_at": dt.datetime(2026, 5, 1, 9, 0, tzinfo=dt.UTC),
        "source_modified_at": dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC),
        "metadata": {},
    }
    row.update(overrides)
    return row


def test_search_returns_drive_documents(connect):
    fake = connect(_FakeConnection(results=[[_drive_row(score=1.5)]]))

    result = DriveV2Client("postgresql://example").search(
        "roadmap", source_type="pdf", occurred_after="2026-05-01"
    )

    assert result["status"] == "ok"
    assert result["search_mode"] == "keyword"
    assert result["count"] == 1
    document = result["results"][0]
    assert document["document_id"] == "google-drive:file-1:0"
    assert document["source"] == "docs"
    assert document["source_type"] == "pdf"
    assert document["source_document_id"] == "file-1"
    assert document["score"] == 1.5
    assert document["metadata"]["drive_id"] == "shared-drive-1"
    assert document["metadata"]["page_start"] == 1
    query, args = fake.queries[0]
    assert "FROM company_context_data.google_drive_documents" in query
    assert args == (
        "roadmap",
        "roadmap",
        "pdf",
        dt.datetime(2026, 5, 1, tzinfo=dt.UTC),
        None,
        10,
    )
    assert fake.closed is True


def test_search_rejects_unknown_source_type(connect):
    fake = connect(_FakeConnection())

    result = DriveV2Client("postgresql://example").search("roadmap", source_type="slack_thread")

    assert result == {"status": "error", "error": "source_type must be one of google_doc, pdf"}
    assert fake.queries == []


def test_read_document_returns_bounded_content(connect):
    connect(_FakeConnection(results=[_drive_row()]))

    result = DriveV2Client("postgresql://example").read_document(
        "google-drive:file-1:0", max_chars=7
    )

    assert result["status"] == "ok"
    assert result["source_type"] == "pdf"
    assert result["content"] == "Roadmap"
    assert result["truncated"] is True


def test_read_document_reports_missing_document(connect):
    connect(_FakeConnection(results=[None]))

    result = DriveV2Client("postgresql://example").read_document("google-drive:missing:0")

    assert result == {"status": "error", "error": "document not found: google-drive:missing:0"}


def test_status_is_active_when_documents_table_is_readable(connect):
    fake = connect(_FakeConnection())

    result = DriveV2Client("postgresql://example").status()

    assert result == {
        "status": "ok",
        "active": True,
        "table": "company_context_data.google_drive_documents",
    }
    assert fake.closed is True


@pytest.mark.parametrize(
    "error",
    [
        asyncpg.UndefinedTableError(
            'relation "company_context_data.google_drive_documents" does not exist'
        ),
        asyncpg.InsufficientPrivilegeError("permission denied for schema company_context_data"),
    ],
)
def test_status_is_inactive_when_documents_table_is_unavailable(connect, error):
    fake = connect(_FakeConnection(results=[error]))

    result = DriveV2Client("postgresql://example").status()

    assert result["status"] == "ok"
    assert result["active"] is False
    assert result["reason"] == str(error)
    assert fake.closed is True
