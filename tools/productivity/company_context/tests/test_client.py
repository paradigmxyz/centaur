from __future__ import annotations

import datetime as dt
import re
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

import client as company_context_client
from client import CompanyContextClient

from centaur_sdk.tool_sdk import ToolContext, reset_tool_context, set_tool_context


class _FakeConnection:
    def __init__(
        self,
        *,
        rows=None,
        fetch_rows=None,
        row=None,
        fetchrow_rows=None,
        val=None,
    ) -> None:
        self.rows = rows or []
        self.fetch_rows = list(fetch_rows or [])
        self.row = row
        self.fetchrow_rows = list(fetchrow_rows or [])
        self.val = val
        self.fetch_calls = []
        self.fetchrow_calls = []
        self.fetchval_calls = []
        self.closed = False

    async def fetch(self, query, *args):
        self.fetch_calls.append((query, args))
        if self.fetch_rows:
            return self.fetch_rows.pop(0)
        return self.rows

    async def fetchrow(self, query, *args):
        self.fetchrow_calls.append((query, args))
        if self.fetchrow_rows:
            return self.fetchrow_rows.pop(0)
        return self.row

    async def fetchval(self, query, *args):
        self.fetchval_calls.append((query, args))
        return self.val

    async def close(self):
        self.closed = True


@pytest.fixture(autouse=True)
def _disable_lookup_metric_push(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_LOOKUP_METRICS_ENABLED", "0")


@pytest.mark.parametrize("query", ["", "   "])
def test_search_rejects_empty_query(query):
    result = CompanyContextClient("postgresql://example").search(query)

    assert result == {"status": "error", "error": "query cannot be empty"}


def test_default_database_url_uses_company_context_dsn_env(monkeypatch):
    monkeypatch.setenv("CENTAUR_POSTGRES_DSN", "postgresql://scoped")
    monkeypatch.setenv("DATABASE_URL", "postgresql://raw-app-db")

    client = CompanyContextClient()

    assert client._require_database_url() == "postgresql://scoped"


def test_default_database_url_uses_tool_context_secret(monkeypatch):
    monkeypatch.delenv("CENTAUR_POSTGRES_DSN", raising=False)
    monkeypatch.setenv("DATABASE_URL", "postgresql://raw-app-db")
    token = set_tool_context(
        ToolContext(
            name="company_context",
            secrets={"CENTAUR_POSTGRES_DSN": "postgresql://context-scoped"},
        )
    )
    try:
        client = CompanyContextClient()

        assert client._require_database_url() == "postgresql://context-scoped"
    finally:
        reset_tool_context(token)


def test_default_database_url_does_not_fall_back_to_raw_database_url(monkeypatch):
    monkeypatch.delenv("CENTAUR_POSTGRES_DSN", raising=False)
    monkeypatch.setenv("DATABASE_URL", "postgresql://raw-app-db")
    token = set_tool_context(ToolContext(name="company_context", secrets={}))
    try:
        client = CompanyContextClient()

        with pytest.raises(RuntimeError, match="CENTAUR_POSTGRES_DSN is required"):
            client._require_database_url()
    finally:
        reset_tool_context(token)


def test_postgres_database_name_defaults_to_ai_v2(monkeypatch):
    monkeypatch.delenv("COMPANY_CONTEXT_POSTGRES_DATABASE", raising=False)

    assert company_context_client._postgres_database_name() == "ai_v2"


def test_postgres_database_name_can_be_overridden(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_POSTGRES_DATABASE", "centaur")

    assert company_context_client._postgres_database_name() == "centaur"


def test_postgres_database_name_uses_default_for_blank_override(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_POSTGRES_DATABASE", " ")

    assert company_context_client._postgres_database_name() == "ai_v2"


def test_search_queries_bm25_and_returns_compact_results(monkeypatch):
    occurred_at = dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC)
    source_updated_at = dt.datetime(2026, 5, 8, 12, 5, tzinfo=dt.UTC)
    fake = _FakeConnection(
        rows=[
            {
                "document_id": "slack:thread:C123:1770000000.000000",
                "source": "slack",
                "source_type": "slack_thread",
                "title": "BM25 indexing plan",
                "url": "https://slack.example/thread",
                "occurred_at": occurred_at,
                "source_updated_at": source_updated_at,
                "metadata": {"channel_name": "eng-ai", "thread_ts": "1770000000.000000"},
                "score": 1.25,
            }
        ],
        row={
            "latest_date": dt.datetime(2026, 5, 10, 15, 30, tzinfo=dt.UTC),
            "latest_source_updated_at": dt.datetime(2026, 5, 10, 15, 30, tzinfo=dt.UTC),
            "latest_occurred_at": dt.datetime(2026, 5, 10, 14, 0, tzinfo=dt.UTC),
            "document_count": 42,
        },
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search(
        "ParadeDB BM25",
        limit=5,
        source="slack",
        source_type="slack_thread",
    )

    assert result["status"] == "ok"
    assert result["count"] == 1
    assert result["indexed_count"] == 1
    assert result["results"][0] == {
        "document_id": "slack:thread:C123:1770000000.000000",
        "source": "slack",
        "source_type": "slack_thread",
        "source_document_id": "",
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": "BM25 indexing plan",
        "url": "https://slack.example/thread",
        "author_name": "",
        "access_scope": "",
        "score": 1.25,
        "preview": "",
        "lane": "indexed",
        "result_type": "slack_thread",
        "occurred_at": "2026-05-08T12:00:00+00:00",
        "source_updated_at": "2026-05-08T12:05:00+00:00",
        "metadata": {"channel_name": "eng-ai", "thread_ts": "1770000000.000000"},
    }
    query, args = fake.fetch_calls[0]
    assert "title ||| $1::text::pdb.boost(8) OR body ||| $1::text::pdb.boost(2)" in query
    assert "title ||| $2::text::pdb.boost(4) OR body ||| $2::text" in query
    assert "title ||| $3::text::pdb.boost(4) OR body ||| $3::text" in query
    assert ") OR (" in query
    assert "WHEN 'slack_thread' THEN 1.25" in query
    assert "WHEN 'slack_channel_day' THEN 0.75" in query
    assert "END DESC" in query
    assert "paradedb.score(document_id)" in query
    assert "metadata ->> 'channel_id'" not in query
    assert args == (
        "ParadeDB BM25",
        "ParadeDB",
        "BM25",
        "slack",
        "slack_thread",
        None,
        None,
        5,
    )
    assert fake.closed is True


def test_search_emits_grouped_lookup_metrics(monkeypatch):
    fake = _FakeConnection(
        rows=[
            {
                "document_id": "slack:thread:C123:1770000000.000000",
                "source": "slack",
                "source_type": "slack_thread",
                "title": "Shopify launch",
                "body": "Shopify launch details",
                "occurred_at": dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC),
                "source_updated_at": dt.datetime(2026, 5, 8, 12, 5, tzinfo=dt.UTC),
                "metadata": {},
                "score": 1.0,
            },
            {
                "document_id": "slack:thread:C123:1770000001.000000",
                "source": "slack",
                "source_type": "slack_thread",
                "title": "More Shopify launch",
                "body": "More Shopify launch details",
                "occurred_at": dt.datetime(2026, 5, 8, 13, 0, tzinfo=dt.UTC),
                "source_updated_at": dt.datetime(2026, 5, 8, 13, 5, tzinfo=dt.UTC),
                "metadata": {},
                "score": 0.9,
            },
            {
                "document_id": "google_drive:doc:launch-plan",
                "source": "google_drive",
                "source_type": "google_doc",
                "title": "Launch plan",
                "body": "Shopify launch plan",
                "occurred_at": dt.datetime(2026, 5, 7, 12, 0, tzinfo=dt.UTC),
                "source_updated_at": dt.datetime(2026, 5, 7, 12, 5, tzinfo=dt.UTC),
                "metadata": {},
                "score": 0.8,
            },
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    pushed_lines = []
    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)
    monkeypatch.setattr(company_context_client, "_include_google_docs_source", lambda *_args: False)
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    result = CompanyContextClient("postgresql://example").search("Shopify launch", limit=10)

    assert result["status"] == "ok"
    assert result["indexed_count"] == 3
    assert any(
        line.startswith(
            'company_context_lookup_requests{requested_source="all",'
            'requested_source_type="all",status="ok",time_window="false"} 1 '
        )
        for line in pushed_lines
    )
    assert any(
        line.startswith(
            'company_context_lookup_results{lane="indexed",source="google_drive",'
            'source_type="google_doc"} 1 '
        )
        for line in pushed_lines
    )
    assert any(
        line.startswith(
            'company_context_lookup_results{lane="indexed",source="slack",'
            'source_type="slack_thread"} 2 '
        )
        for line in pushed_lines
    )
    assert not any(line.startswith("company_context_lookup_zero_results") for line in pushed_lines)


def test_search_emits_zero_result_lookup_metric(monkeypatch):
    fake = _FakeConnection(rows=[])

    async def fake_connect(*args, **kwargs):
        return fake

    pushed_lines = []
    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    result = CompanyContextClient("postgresql://example").search(
        "missing launch",
        source="slack",
        source_type="slack_thread",
        occurred_after="2026-05-01",
    )

    assert result["status"] == "ok"
    assert result["indexed_count"] == 0
    assert any(
        line.startswith(
            'company_context_lookup_zero_results{requested_source="slack",'
            'requested_source_type="slack_thread",status="ok",time_window="true"} 1 '
        )
        for line in pushed_lines
    )


def test_search_emits_error_lookup_metric(monkeypatch):
    async def fake_connect(*args, **kwargs):
        raise RuntimeError("database unavailable")

    pushed_lines = []
    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    result = CompanyContextClient("postgresql://example").search(
        "Shopify launch",
        source="google_drive",
    )

    assert result == {"status": "error", "error": "database unavailable"}
    assert pushed_lines
    assert any(
        line.startswith(
            'company_context_lookup_requests{requested_source="google_drive",'
            'requested_source_type="all",status="error",time_window="false"} 1 '
        )
        for line in pushed_lines
    )


def test_search_uses_or_terms_and_drops_stop_words(monkeypatch):
    fake = _FakeConnection(rows=[])

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search(
        "what is the state root state mismatch in prod",
        limit=3,
    )

    assert result["status"] == "ok"
    query, args = fake.fetch_calls[0]
    assert "WHERE (title ||| $1::text::pdb.boost(8) OR body ||| $1::text::pdb.boost(2))" in query
    assert "OR (title ||| $2::text::pdb.boost(4) OR body ||| $2::text)" in query
    assert "OR (title ||| $3::text::pdb.boost(4) OR body ||| $3::text)" in query
    assert "OR (title ||| $4::text::pdb.boost(4) OR body ||| $4::text)" in query
    assert "OR (title ||| $5::text::pdb.boost(4) OR body ||| $5::text)" in query
    assert "title ||| $6::text::pdb.boost(4)" not in query
    placeholders = {int(match.group(1)) for match in re.finditer(r"\$(\d+)", query)}
    assert placeholders == set(range(1, len(args) + 1))
    assert "metadata ->> 'channel_id'" not in query
    assert args == (
        "what is the state root state mismatch in prod",
        "state",
        "root",
        "mismatch",
        "prod",
        None,
        None,
        None,
        None,
        3,
    )


def test_search_applies_occurred_at_filters(monkeypatch):
    fake = _FakeConnection(rows=[])

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search(
        "planning",
        limit=4,
        source="google_calendar",
        source_type="calendar_event",
        occurred_after="2026-05-01",
        occurred_before="2026-05-08T12:30:00Z",
    )

    assert result["status"] == "ok"
    assert result["occurred_after"] == "2026-05-01T00:00:00+00:00"
    assert result["occurred_before"] == "2026-05-08T12:30:00+00:00"
    query, args = fake.fetch_calls[0]
    assert "OR occurred_at >= $5" in query
    assert "OR occurred_at < $6" in query
    assert "metadata ->> 'channel_id'" not in query
    assert args == (
        "planning",
        "planning",
        "google_calendar",
        "calendar_event",
        dt.datetime(2026, 5, 1, tzinfo=dt.UTC),
        dt.datetime(2026, 5, 8, 12, 30, tzinfo=dt.UTC),
        4,
    )


def test_search_docs_source_queries_legacy_drive_and_oauth_docs_indexes(monkeypatch):
    created_at = dt.datetime(2026, 5, 1, 9, 0, tzinfo=dt.UTC)
    modified_at = dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC)
    fake = _FakeConnection(
        fetch_rows=[
            [
                {
                    "document_id": "google_drive:doc:legacy-doc-1",
                    "source": "google_drive",
                    "source_type": "google_doc",
                    "source_document_id": "legacy-doc-1",
                    "source_chunk_id": "",
                    "parent_document_id": None,
                    "title": "Legacy roadmap notes",
                    "body": "Legacy Drive projection mentions launch sequencing.",
                    "url": "https://docs.google.com/document/d/legacy-doc-1/edit",
                    "author_name": "Bob",
                    "access_scope": "",
                    "occurred_at": created_at,
                    "source_updated_at": modified_at,
                    "metadata": {"drive_id": "drive-legacy"},
                    "score": 1.5,
                }
            ],
            [
                {
                    "document_id": "google_docs:doc-123:chunk-0000",
                    "file_id": "doc-123",
                    "chunk_id": "chunk-0000",
                    "title": "Roadmap notes",
                    "body": "Roadmap notes mention launch sequencing and onboarding.",
                    "url": "https://docs.google.com/document/d/doc-123/edit",
                    "provider_author_id": "perm-1",
                    "provider_author_name": "Alice",
                    "mime_type": "application/vnd.google-apps.document",
                    "drive_id": "drive-1",
                    "source_created_at": created_at,
                    "source_modified_at": modified_at,
                    "metadata": {"provider_email": "alice@example.com"},
                    "score": 2.5,
                }
            ],
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search(
        "roadmap",
        limit=3,
        source="docs",
        source_type="google_doc",
        occurred_after="2026-05-01",
        occurred_before="2026-05-09",
    )

    assert result["status"] == "ok"
    assert result["source"] == "docs"
    assert result["source_type"] == "google_doc"
    assert result["count"] == 2
    assert result["indexed_count"] == 2
    assert "google_docs_error" not in result
    assert result["results"][0]["source"] == "docs"
    assert result["results"][0]["document_id"] == "google_docs:doc-123:chunk-0000"
    assert result["results"][1]["source"] == "google_drive"
    assert result["results"][1]["document_id"] == "google_drive:doc:legacy-doc-1"
    legacy_query, legacy_args = fake.fetch_calls[0]
    oauth_query, oauth_args = fake.fetch_calls[1]
    assert "FROM company_context_documents" in legacy_query
    assert legacy_args == (
        "roadmap",
        "roadmap",
        "google_drive",
        "google_doc",
        dt.datetime(2026, 5, 1, tzinfo=dt.UTC),
        dt.datetime(2026, 5, 9, tzinfo=dt.UTC),
        3,
    )
    assert "FROM google_docs_context_documents" in oauth_query
    assert "source_modified_at >= $3" in oauth_query
    assert "source_modified_at < $4" in oauth_query
    assert oauth_args == (
        "roadmap",
        "roadmap",
        dt.datetime(2026, 5, 1, tzinfo=dt.UTC),
        dt.datetime(2026, 5, 9, tzinfo=dt.UTC),
        3,
    )
    assert fake.closed is True


def test_search_drive_source_is_not_docs_alias(monkeypatch):
    fake = _FakeConnection(rows=[])

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search("roadmap", source="drive")

    assert result["status"] == "ok"
    assert len(fake.fetch_calls) == 1
    query, args = fake.fetch_calls[0]
    assert "FROM company_context_documents" in query
    assert "FROM google_docs_context_documents" not in query
    assert args == ("roadmap", "roadmap", "drive", None, None, None, 10)
    assert fake.closed is True


def test_search_rejects_invalid_occurred_at_filter():
    result = CompanyContextClient("postgresql://example").search(
        "planning",
        occurred_after="not-a-date",
    )

    assert result == {
        "status": "error",
        "error": "occurred_after must be an ISO 8601 date or timestamp",
    }


def test_search_rejects_inverted_occurred_at_filter():
    result = CompanyContextClient("postgresql://example").search(
        "planning",
        occurred_after="2026-05-08",
        occurred_before="2026-05-01",
    )

    assert result == {
        "status": "error",
        "error": "occurred_after must be earlier than occurred_before",
    }


@pytest.mark.parametrize("query", ["", "   "])
def test_search_dms_rejects_empty_query(query):
    result = CompanyContextClient("postgresql://example").search_dms(query)

    assert result == {"status": "error", "error": "query cannot be empty"}


@pytest.mark.parametrize("query", ["", "   "])
def test_search_dm_conversations_rejects_empty_query(query):
    result = CompanyContextClient("postgresql://example").search_dm_conversations(query)

    assert result == {"status": "error", "error": "query cannot be empty"}


def test_search_dm_conversations_queries_projection(monkeypatch):
    last_seen_at = dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC)
    source_updated_at = dt.datetime(2026, 5, 8, 12, 5, tzinfo=dt.UTC)
    fake = _FakeConnection(
        rows=[
            {
                "document_id": "slack_dm_conversation:T_HOME:D123",
                "home_team_id": "T_HOME",
                "conversation_id": "D123",
                "conversation_type": "im",
                "title": "Slack DM: Akshaan, Tom",
                "body": "D123 U_SELF Akshaan U_TOM Tom tom@example.com",
                "is_ext_shared": False,
                "last_seen_at": last_seen_at,
                "source_updated_at": source_updated_at,
                "participant_user_ids": ["U_SELF", "U_TOM"],
                "participant_labels": ["Akshaan", "Tom"],
                "participant_count": 2,
                "metadata": {"source": "slack_dm_conversation"},
                "score": 3.25,
            }
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search_dm_conversations(
        " Tom ",
        limit=500,
    )

    assert result == {
        "status": "ok",
        "query": "Tom",
        "source": "slack_dm",
        "count": 1,
        "results": [
            {
                "document_id": "slack_dm_conversation:T_HOME:D123",
                "source": "slack_dm",
                "source_type": "slack_dm_conversation",
                "home_team_id": "T_HOME",
                "conversation_id": "D123",
                "conversation_type": "im",
                "title": "Slack DM: Akshaan, Tom",
                "is_ext_shared": False,
                "last_seen_at": "2026-05-08T12:00:00+00:00",
                "source_updated_at": "2026-05-08T12:05:00+00:00",
                "participant_user_ids": ["U_SELF", "U_TOM"],
                "participant_labels": ["Akshaan", "Tom"],
                "participant_count": 2,
                "matched_labels": ["Tom"],
                "metadata": {"source": "slack_dm_conversation"},
                "score": 3.25,
                "preview": "D123 U_SELF Akshaan U_TOM Tom tom@example.com",
            }
        ],
    }
    query, args = fake.fetch_calls[0]
    assert "FROM slack_dm_conversation_context_documents" in query
    assert "title ||| $1::text::pdb.boost(8) OR body ||| $1::text::pdb.boost(2)" in query
    assert "OR (title ||| $2::text::pdb.boost(4) OR body ||| $2::text)" in query
    assert "LIMIT $3" in query
    assert "centaur_search_slack_dm_conversations" not in query
    assert args == ("Tom", "Tom", 50)
    assert fake.closed is True


def test_search_dms_queries_bm25_and_returns_compact_results(monkeypatch):
    occurred_at = dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC)
    source_updated_at = dt.datetime(2026, 5, 8, 12, 5, tzinfo=dt.UTC)
    fake = _FakeConnection(
        rows=[
            {
                "document_id": "slack_dm:T_HOME:D123:1770000000.000000",
                "home_team_id": "T_HOME",
                "conversation_id": "D123",
                "message_ts": "1770000000.000000",
                "conversation_type": "im",
                "thread_ts": None,
                "user_id": "U123",
                "bot_id": "",
                "title": "Slack DM",
                "body": "launch plan\nAlpha attachment",
                "permalink": "https://slack.example/archives/D123/p1770000000000000",
                "occurred_at": occurred_at,
                "source_updated_at": source_updated_at,
                "metadata": {"attachment_count": 1, "conversation_type": "im"},
                "score": 2.5,
            }
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search_dms(
        "launch plan",
        limit=5,
        conversation_id=" D123 ",
    )

    assert result == {
        "status": "ok",
        "query": "launch plan",
        "source": "slack_dm",
        "conversation_id": "D123",
        "occurred_after": None,
        "occurred_before": None,
        "count": 1,
        "results": [
            {
                "document_id": "slack_dm:T_HOME:D123:1770000000.000000",
                "source": "slack_dm",
                "source_type": "slack_im",
                "source_document_id": "D123",
                "source_chunk_id": "1770000000.000000",
                "parent_document_id": None,
                "title": "Slack DM",
                "url": "https://slack.example/archives/D123/p1770000000000000",
                "author_name": "U123",
                "access_scope": "slack_dm",
                "occurred_at": "2026-05-08T12:00:00+00:00",
                "source_updated_at": "2026-05-08T12:05:00+00:00",
                "conversation_id": "D123",
                "conversation_type": "im",
                "message_ts": "1770000000.000000",
                "thread_ts": None,
                "user_id": "U123",
                "bot_id": "",
                "attachment_count": 1,
                "metadata": {"attachment_count": 1, "conversation_type": "im"},
                "score": 2.5,
                "preview": "launch plan Alpha attachment",
                "lane": "indexed",
                "result_type": "slack_im",
            }
        ],
    }
    query, args = fake.fetch_calls[0]
    assert "FROM slack_dm_context_documents" in query
    assert "title ||| $1::text::pdb.boost(8) OR body ||| $1::text::pdb.boost(2)" in query
    assert "OR (title ||| $2::text::pdb.boost(4) OR body ||| $2::text)" in query
    assert "OR (title ||| $3::text::pdb.boost(4) OR body ||| $3::text)" in query
    assert "conversation_id = $4" in query
    assert "OR occurred_at >= $5" in query
    assert "OR occurred_at < $6" in query
    assert "LIMIT $7" in query
    assert "centaur.slack_user_id" not in query
    assert "centaur.slack_team_id" not in query
    assert args == ("launch plan", "launch", "plan", "D123", None, None, 5)
    assert fake.closed is True


def test_search_dms_applies_occurred_at_filters(monkeypatch):
    fake = _FakeConnection(rows=[])

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").search_dms(
        "planning",
        limit=4,
        occurred_after="2026-05-01",
        occurred_before="2026-05-08T12:30:00Z",
    )

    assert result["status"] == "ok"
    assert result["occurred_after"] == "2026-05-01T00:00:00+00:00"
    assert result["occurred_before"] == "2026-05-08T12:30:00+00:00"
    _, args = fake.fetch_calls[0]
    assert args == (
        "planning",
        "planning",
        None,
        dt.datetime(2026, 5, 1, tzinfo=dt.UTC),
        dt.datetime(2026, 5, 8, 12, 30, tzinfo=dt.UTC),
        4,
    )


def test_search_dms_rejects_invalid_occurred_at_filter():
    result = CompanyContextClient("postgresql://example").search_dms(
        "planning",
        occurred_after="not-a-date",
    )

    assert result == {
        "status": "error",
        "error": "occurred_after must be an ISO 8601 date or timestamp",
    }


def test_search_dms_rejects_inverted_occurred_at_filter():
    result = CompanyContextClient("postgresql://example").search_dms(
        "planning",
        occurred_after="2026-05-08",
        occurred_before="2026-05-01",
    )

    assert result == {
        "status": "error",
        "error": "occurred_after must be earlier than occurred_before",
    }


def test_list_documents_returns_date_bounded_document_summaries(monkeypatch):
    fake = _FakeConnection(
        rows=[
            {
                "document_id": "google_calendar:calendar_event:evt_123",
                "source": "google_calendar",
                "source_type": "calendar_event",
                "source_document_id": "evt_123",
                "source_chunk_id": "",
                "parent_document_id": None,
                "title": "Planning sync",
                "url": "https://calendar.example/event",
                "author_name": "alice",
                "access_scope": "company",
                "body": "Planning sync with roadmap notes.",
                "occurred_at": dt.datetime(2026, 5, 6, 15, 0, tzinfo=dt.UTC),
                "source_updated_at": dt.datetime(2026, 5, 6, 15, 30, tzinfo=dt.UTC),
                "metadata": {"calendar_id": "primary"},
            }
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").list_documents(
        limit=2,
        source="google_calendar",
        source_type="calendar_event",
        occurred_after="2026-05-01",
        occurred_before="2026-05-08",
    )

    assert result == {
        "status": "ok",
        "source": "google_calendar",
        "source_type": "calendar_event",
        "occurred_after": "2026-05-01T00:00:00+00:00",
        "occurred_before": "2026-05-08T00:00:00+00:00",
        "count": 1,
        "results": [
            {
                "document_id": "google_calendar:calendar_event:evt_123",
                "source": "google_calendar",
                "source_type": "calendar_event",
                "source_document_id": "evt_123",
                "source_chunk_id": "",
                "parent_document_id": None,
                "title": "Planning sync",
                "url": "https://calendar.example/event",
                "author_name": "alice",
                "access_scope": "company",
                "occurred_at": "2026-05-06T15:00:00+00:00",
                "source_updated_at": "2026-05-06T15:30:00+00:00",
                "metadata": {"calendar_id": "primary"},
                "preview": "Planning sync with roadmap notes.",
            }
        ],
    }
    query, args = fake.fetch_calls[0]
    assert "ORDER BY occurred_at ASC NULLS LAST" in query
    assert "metadata ->> 'channel_id'" not in query
    assert args == (
        "google_calendar",
        "calendar_event",
        dt.datetime(2026, 5, 1, tzinfo=dt.UTC),
        dt.datetime(2026, 5, 8, tzinfo=dt.UTC),
        2,
    )
    assert fake.closed is True


def test_latest_date_returns_latest_indexed_slack_timestamp(monkeypatch):
    fake = _FakeConnection(
        row={
            "latest_date": dt.datetime(2026, 5, 10, 15, 30, tzinfo=dt.UTC),
            "latest_source_updated_at": dt.datetime(2026, 5, 10, 15, 30, tzinfo=dt.UTC),
            "latest_occurred_at": dt.datetime(2026, 5, 10, 14, 0, tzinfo=dt.UTC),
            "document_count": 42,
        }
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").latest_date(
        source="slack",
        source_type="slack_thread",
    )

    assert result == {
        "status": "ok",
        "source": "slack",
        "source_type": "slack_thread",
        "document_count": 42,
        "latest_date": "2026-05-10T15:30:00+00:00",
        "latest_source_updated_at": "2026-05-10T15:30:00+00:00",
        "latest_occurred_at": "2026-05-10T14:00:00+00:00",
    }
    _, args = fake.fetchrow_calls[0]
    assert args == ("slack", "slack_thread")
    assert fake.closed is True


def test_latest_date_reports_empty_index(monkeypatch):
    fake = _FakeConnection(
        row={
            "latest_date": None,
            "latest_source_updated_at": None,
            "latest_occurred_at": None,
            "document_count": 0,
        }
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").latest_date(source="slack")

    assert result == {
        "status": "ok",
        "source": "slack",
        "source_type": None,
        "document_count": 0,
        "latest_date": None,
        "latest_source_updated_at": None,
        "latest_occurred_at": None,
    }
    assert fake.closed is True


def test_read_document_returns_full_content_by_default(monkeypatch):
    body = "x" * 2_500
    fake = _FakeConnection(
        row={
            "document_id": "slack:channel_day:C123:2026-05-08",
            "source": "slack",
            "source_type": "slack_channel_day",
            "title": "#eng-ai - 2026-05-08",
            "body": body,
            "url": "",
            "occurred_at": None,
            "source_updated_at": None,
            "metadata": '{"channel_name": "eng-ai"}',
        }
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").read_document(
        " slack:channel_day:C123:2026-05-08 ",
    )

    assert result["status"] == "ok"
    assert result["document_id"] == "slack:channel_day:C123:2026-05-08"
    assert result["chars"] == 2_500
    assert result["total_chars"] == 2_500
    assert result["truncated"] is False
    assert result["content"] == body
    assert result["metadata"] == {"channel_name": "eng-ai"}
    _, args = fake.fetchrow_calls[0]
    assert args == ("slack:channel_day:C123:2026-05-08",)
    assert fake.closed is True


def test_read_document_can_return_bounded_content(monkeypatch):
    body = "x" * 2_500
    fake = _FakeConnection(
        row={
            "document_id": "slack:channel_day:C123:2026-05-08",
            "source": "slack",
            "source_type": "slack_channel_day",
            "title": "#eng-ai - 2026-05-08",
            "body": body,
            "url": "",
            "occurred_at": None,
            "source_updated_at": None,
            "metadata": '{"channel_name": "eng-ai"}',
        }
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").read_document(
        "slack:channel_day:C123:2026-05-08",
        max_chars=1_200,
    )

    assert result["status"] == "ok"
    assert result["document_id"] == "slack:channel_day:C123:2026-05-08"
    assert result["chars"] == 1_200
    assert result["total_chars"] == 2_500
    assert result["truncated"] is True
    assert result["content"] == "x" * 1_200
    assert result["metadata"] == {"channel_name": "eng-ai"}
    _, args = fake.fetchrow_calls[0]
    assert args == ("slack:channel_day:C123:2026-05-08",)
    assert fake.closed is True


def test_read_document_falls_back_to_oauth_google_docs_index(monkeypatch):
    created_at = dt.datetime(2026, 5, 1, 9, 0, tzinfo=dt.UTC)
    modified_at = dt.datetime(2026, 5, 8, 12, 0, tzinfo=dt.UTC)
    body = "OAuth Google Doc content"
    fake = _FakeConnection(
        fetchrow_rows=[
            None,
            {
                "document_id": "google_docs:doc-123:chunk-0000",
                "file_id": "doc-123",
                "chunk_id": "chunk-0000",
                "title": "Roadmap notes",
                "body": body,
                "url": "https://docs.google.com/document/d/doc-123/edit",
                "provider_author_id": "perm-1",
                "provider_author_name": "Alice",
                "mime_type": "application/vnd.google-apps.document",
                "drive_id": "drive-1",
                "source_created_at": created_at,
                "source_modified_at": modified_at,
                "metadata": {"provider_email": "alice@example.com"},
            },
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").read_document(
        "google_docs:doc-123:chunk-0000",
        max_chars=10,
    )

    assert result["status"] == "ok"
    assert result["source"] == "docs"
    assert result["source_type"] == "google_doc"
    assert result["source_document_id"] == "doc-123"
    assert result["content"] == "OAuth Goog"
    assert result["chars"] == 10
    assert result["total_chars"] == len(body)
    assert result["truncated"] is True
    assert len(fake.fetchrow_calls) == 2
    assert fake.closed is True


def test_read_document_reports_missing_document(monkeypatch):
    fake = _FakeConnection(row=None)

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(company_context_client.asyncpg, "connect", fake_connect)

    result = CompanyContextClient("postgresql://example").read_document("missing-doc")

    assert result == {"status": "error", "error": "document not found: missing-doc"}
