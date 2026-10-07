from __future__ import annotations

import datetime as dt
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from company_context.drive_v2 import DriveV2Client
from conftest import embedding


class _FakeOpenAIClient:
    def __init__(self, vector) -> None:
        async def create(**_kwargs):
            return SimpleNamespace(data=[SimpleNamespace(embedding=vector)])

        self.embeddings = SimpleNamespace(create=create)


@pytest.fixture(autouse=True)
def _disable_embeddings(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "false")


def _add_roadmap(database, **overrides):
    values = {
        "file_id": "file-1",
        "title": "Roadmap.pdf",
        "body": "Roadmap PDF covers launch sequencing.",
        "created_at": dt.datetime(2026, 5, 1, 9, 0, tzinfo=dt.UTC),
        "modified_at": dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC),
    }
    values.update(overrides)
    database.add_drive_document(f"google-drive:{values['file_id']}:0", **values)


def test_search_returns_visible_drive_documents(paradedb_database):
    _add_roadmap(paradedb_database)
    _add_roadmap(paradedb_database, file_id="file-other", subject="subject-other")
    _add_roadmap(paradedb_database, file_id="file-doc", document_type="google_doc")

    result = DriveV2Client(paradedb_database.dsn).search(
        "roadmap", source_type="pdf", occurred_after="2026-05-01"
    )

    assert result["status"] == "ok"
    assert result["search_mode"] == "keyword"
    assert [document["document_id"] for document in result["results"]] == ["google-drive:file-1:0"]
    document = result["results"][0]
    assert document["source"] == "docs"
    assert document["source_type"] == "pdf"
    assert document["source_document_id"] == "file-1"
    assert document["score"] > 0
    assert document["metadata"]["drive_id"] == "shared-drive"
    assert document["metadata"]["page_start"] == 1


def test_search_fuses_vector_results_through_iron_proxy(paradedb_database, monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    _add_roadmap(paradedb_database, embedding=embedding(1.0))
    _add_roadmap(
        paradedb_database,
        file_id="file-vector",
        title="Plan.pdf",
        body="Sequencing for the quarter.",
        embedding=embedding(0.9, 0.1),
    )

    result = DriveV2Client(
        paradedb_database.dsn,
        embeddings_client=_FakeOpenAIClient(embedding(1.0)),
    ).search("roadmap")

    assert "vector_error" not in result
    assert result["search_mode"] == "hybrid"
    assert [(document["document_id"], document["lane"]) for document in result["results"]] == [
        ("google-drive:file-1:0", "hybrid"),
        ("google-drive:file-vector:0", "vector"),
    ]


def test_search_rejects_unknown_source_type():
    result = DriveV2Client("postgresql://example").search("roadmap", source_type="slack_thread")

    assert result == {"status": "error", "error": "source_type must be one of google_doc, pdf"}


def test_read_document_returns_bounded_content(paradedb_database):
    _add_roadmap(paradedb_database)

    result = DriveV2Client(paradedb_database.dsn).read_document(
        "google-drive:file-1:0", max_chars=7
    )

    assert result["status"] == "ok"
    assert result["source_type"] == "pdf"
    assert result["content"] == "Roadmap"
    assert result["truncated"] is True


def test_read_document_reports_missing_and_hidden_documents(paradedb_database):
    _add_roadmap(paradedb_database, file_id="file-other", subject="subject-other")
    client = DriveV2Client(paradedb_database.dsn)

    for document_id in ("google-drive:missing:0", "google-drive:file-other:0"):
        assert client.read_document(document_id) == {
            "status": "error",
            "error": f"document not found: {document_id}",
        }


def test_status_is_active_when_documents_table_is_readable(paradedb_database):
    result = DriveV2Client(paradedb_database.dsn).status()

    assert result == {
        "status": "ok",
        "active": True,
        "table": "company_context_data.google_drive_documents",
    }


def test_status_is_inactive_without_the_company_context_service(postgres_database):
    result = DriveV2Client(postgres_database.dsn).status()

    assert result["status"] == "ok"
    assert result["active"] is False
    assert "company_context_data.google_drive_documents" in result["reason"]
