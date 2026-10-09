from __future__ import annotations

import datetime as dt
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

import client as company_context_client
from client import CompanyContextClient
from conftest import CHANNEL_ID, EMBEDDINGS_MODEL, embedding

from centaur_sdk.tool_sdk import ToolContext, reset_tool_context, set_tool_context

UNREACHABLE_DSN = "postgresql://sandbox:unused@127.0.0.1:1/ai_v2"


def _at(day: int, hour: int = 12, minute: int = 0, month: int = 5) -> dt.datetime:
    return dt.datetime(2026, month, day, hour, minute, tzinfo=dt.UTC)


class _FakeEmbeddingsAPI:
    def __init__(self, vector=None, error: Exception | None = None) -> None:
        self.vector = vector or embedding(1.0)
        self.error = error
        self.calls = []

    async def create(self, **kwargs):
        self.calls.append(kwargs)
        if self.error:
            raise self.error
        return SimpleNamespace(data=[SimpleNamespace(embedding=self.vector)])


class _FakeOpenAIClient:
    def __init__(self, vector=None, error: Exception | None = None) -> None:
        self.embeddings = _FakeEmbeddingsAPI(vector=vector, error=error)


@pytest.fixture(autouse=True)
def _disable_lookup_metric_push(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_LOOKUP_METRICS_ENABLED", "0")
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "false")


DM_CONVERSATION_FIELDS = (
    "source",
    "source_type",
    "home_team_id",
    "conversation_id",
    "conversation_type",
    "title",
    "is_ext_shared",
    "last_seen_at",
    "participant_user_ids",
    "participant_labels",
    "participant_count",
    "matched_labels",
)
DM_MESSAGE_FIELDS = (
    "source",
    "source_type",
    "source_document_id",
    "source_chunk_id",
    "parent_document_id",
    "title",
    "url",
    "author_name",
    "access_scope",
    "occurred_at",
    "conversation_id",
    "conversation_type",
    "message_ts",
    "thread_ts",
    "user_id",
    "bot_id",
    "attachment_count",
    "preview",
    "lane",
    "result_type",
)


def _ids(result: dict) -> list[str]:
    assert result["status"] == "ok", result
    return [item["document_id"] for item in result["results"]]


@pytest.mark.parametrize("query", ["", "   "])
def test_search_rejects_empty_query(query):
    result = CompanyContextClient("postgresql://example").search(query)

    assert result == {"status": "error", "error": "query cannot be empty"}


def test_default_database_url_uses_company_context_dsn_env(monkeypatch):
    monkeypatch.setenv("CENTAUR_POSTGRES_DSN", "postgresql://scoped")
    monkeypatch.setenv("DATABASE_URL", "postgresql://raw-app-db")

    client = CompanyContextClient()

    assert client._require_database_url() == "postgresql://scoped/ai_v2"


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

        assert client._require_database_url() == "postgresql://context-scoped/ai_v2"
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


@pytest.mark.parametrize("sql", ["", "   "])
def test_query_rejects_empty_sql(sql):
    result = CompanyContextClient("postgresql://example").query(sql)

    assert result == {"status": "error", "error": "sql cannot be empty"}


def test_query_returns_bounded_results(database):
    for document_id in ("doc-a", "doc-b", "doc-c"):
        database.add_document(document_id, title=document_id)

    result = CompanyContextClient(database.dsn).query(
        "SELECT document_id, title FROM company_context_documents ORDER BY document_id;",
        limit=2,
    )

    assert result == {
        "status": "ok",
        "row_count": 2,
        "limit": 2,
        "truncated": True,
        "columns": ["document_id", "title"],
        "rows": [
            {"document_id": "doc-a", "title": "doc-a"},
            {"document_id": "doc-b", "title": "doc-b"},
        ],
    }


def test_query_clamps_limit(database):
    result = CompanyContextClient(database.dsn).query("SELECT 1 AS value", limit=10_000)

    assert result["status"] == "ok"
    assert result["limit"] == 1_000
    assert result["rows"] == [{"value": 1}]


def test_query_is_read_only(database):
    database.add_document("doc-a")
    client = CompanyContextClient(database.dsn)

    result = client.query("UPDATE company_context_documents SET title = 'changed'")

    assert result["status"] == "error"
    assert "read-only" in result["error"]
    assert client.query("SELECT title FROM company_context_documents")["rows"] == [{"title": ""}]


def test_query_enforces_timeout(database):
    started = time.monotonic()

    result = CompanyContextClient(database.dsn).query("SELECT pg_sleep(10)", timeout_seconds=1)

    assert result["status"] == "error"
    assert time.monotonic() - started < 5


def test_reader_sees_only_documents_in_visible_slack_channels(database):
    database.add_document("slack:visible", title="Launch plan")
    database.add_slack_channel("C_SECRET", is_private=True)
    database.add_document("slack:secret", title="Launch plan", channel_id="C_SECRET")
    database.add_document(
        "linear:issue",
        title="Launch plan",
        source="linear",
        source_type="linear_issue",
    )
    client = CompanyContextClient(database.dsn)

    assert _ids(client.search("launch plan")) == ["slack:visible"]
    assert client.query("SELECT document_id FROM company_context_documents")["rows"] == [
        {"document_id": "slack:visible"}
    ]


def test_search_returns_compact_results(database):
    database.add_document(
        "slack:thread:C_HOME:1770000000.000000",
        title="BM25 indexing plan",
        body="ParadeDB BM25 indexing   plan\nfor search.",
        source_document_id=CHANNEL_ID,
        url="https://slack.example/thread",
        occurred_at=_at(8),
        source_updated_at=_at(8, minute=5),
        metadata={"channel_name": "eng-ai", "thread_ts": "1770000000.000000"},
    )

    result = CompanyContextClient(database.dsn).search(
        "ParadeDB BM25",
        limit=5,
        source="slack",
        source_type="slack_thread",
    )

    assert result["status"] == "ok"
    assert result["search_mode"] == "keyword"
    assert result["count"] == 1
    assert result["indexed_count"] == 1
    item = result["results"][0]
    assert item.pop("score") > 0
    assert item == {
        "document_id": "slack:thread:C_HOME:1770000000.000000",
        "source": "slack",
        "source_type": "slack_thread",
        "source_document_id": CHANNEL_ID,
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": "BM25 indexing plan",
        "url": "https://slack.example/thread",
        "author_name": "",
        "access_scope": "company",
        "preview": "ParadeDB BM25 indexing plan for search.",
        "lane": "indexed",
        "result_type": "slack_thread",
        "occurred_at": "2026-05-08T12:00:00+00:00",
        "source_updated_at": "2026-05-08T12:05:00+00:00",
        "metadata": {
            "channel_id": CHANNEL_ID,
            "channel_name": "eng-ai",
            "thread_ts": "1770000000.000000",
        },
    }


def test_search_matches_any_content_term(database):
    database.add_document("slack:mismatch", body="Saw a mismatch after the deploy.")
    database.add_document("slack:unrelated", body="Lunch at noon.")

    result = CompanyContextClient(database.dsn).search(
        "what is the state root state mismatch in prod",
        limit=3,
    )

    assert _ids(result) == ["slack:mismatch"]


def test_search_ranks_threads_above_channel_days(database):
    body = "Rollout of the billing migration."
    database.add_document("slack:day", source_type="slack_channel_day", body=body)
    database.add_document("slack:thread", source_type="slack_thread", body=body)

    assert _ids(CompanyContextClient(database.dsn).search("billing migration")) == [
        "slack:thread",
        "slack:day",
    ]


def test_search_applies_source_and_occurred_at_filters(database):
    database.add_document("slack:match", body="Planning sync", occurred_at=_at(6))
    database.add_document("slack:too-early", body="Planning sync", occurred_at=_at(30, month=4))
    database.add_document("slack:too-late", body="Planning sync", occurred_at=_at(8, 12, 30))
    database.add_document(
        "slack:other-type",
        body="Planning sync",
        source_type="slack_channel_day",
        occurred_at=_at(6),
    )

    result = CompanyContextClient(database.dsn).search(
        "planning",
        limit=4,
        source="slack",
        source_type="slack_thread",
        occurred_after="2026-05-01",
        occurred_before="2026-05-08T12:30:00Z",
    )

    assert result["occurred_after"] == "2026-05-01T00:00:00+00:00"
    assert result["occurred_before"] == "2026-05-08T12:30:00+00:00"
    assert _ids(result) == ["slack:match"]


def test_search_docs_source_queries_visible_google_docs(database):
    database.add_google_doc(
        "google_docs:doc-123:0",
        file_id="doc-123",
        title="Roadmap notes",
        body="Roadmap notes mention launch sequencing.",
        created_at=_at(1, 9),
        modified_at=_at(8),
    )
    database.add_google_doc(
        "google_docs:doc-other:0",
        file_id="doc-other",
        title="Roadmap notes",
        body="Roadmap for someone else.",
        subject="subject-other",
        modified_at=_at(8),
    )
    database.add_google_doc(
        "google_docs:doc-old:0",
        file_id="doc-old",
        title="Roadmap notes",
        body="Old roadmap.",
        modified_at=_at(1, month=4),
    )

    result = CompanyContextClient(database.dsn).search(
        "roadmap",
        limit=3,
        source="docs",
        source_type="google_doc",
        occurred_after="2026-05-01",
        occurred_before="2026-05-09",
    )

    assert result["source"] == "docs"
    assert "google_docs_error" not in result
    assert _ids(result) == ["google_docs:doc-123:0"]
    item = result["results"][0]
    assert item["source"] == "docs"
    assert item["source_type"] == "google_doc"
    assert item["source_document_id"] == "doc-123"
    assert item["author_name"] == "Alice"
    assert item["occurred_at"] == "2026-05-01T09:00:00+00:00"
    assert item["source_updated_at"] == "2026-05-08T12:00:00+00:00"


def test_search_drive_source_is_not_docs_alias(database):
    database.add_google_doc("google_docs:doc-123:0", file_id="doc-123", body="Roadmap notes")

    assert _ids(CompanyContextClient(database.dsn).search("roadmap", source="drive")) == []


def test_search_granola_source_returns_notes_shared_with_user(database):
    database.add_granola_note(
        "not_123",
        title="Launch review",
        body="We agreed to ship the launch.",
        occurred_at=_at(1, 10, month=7),
    )
    database.add_granola_note(
        "not_private",
        title="Launch review",
        body="Private launch review.",
        access_emails=("other@example.com",),
        occurred_at=_at(1, 11, month=7),
    )

    result = CompanyContextClient(database.dsn).search(
        "launch review",
        source="granola",
        occurred_after="2026-07-01",
        occurred_before="2026-07-02",
    )

    assert _ids(result) == ["granola:note:not_123"]
    item = result["results"][0]
    assert item["source"] == "granola"
    assert item["source_type"] == "granola_note"
    assert item["source_document_id"] == "not_123"
    assert item["author_name"] == "Alice"


def test_search_skips_embeddings_when_disabled(database):
    database.add_document("slack:keyword", body="keyword query", embedding=embedding(1.0))
    embeddings_client = _FakeOpenAIClient()

    result = CompanyContextClient(database.dsn, embeddings_client=embeddings_client).search(
        "keyword query",
        source="slack",
    )

    assert result["search_mode"] == "keyword"
    assert result["vector_count"] == 0
    assert _ids(result) == ["slack:keyword"]
    assert embeddings_client.embeddings.calls == []


def test_search_hybrid_override_forces_keyword_search(database, monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    database.add_document("slack:keyword", body="keyword query", embedding=embedding(1.0))
    embeddings_client = _FakeOpenAIClient()

    result = CompanyContextClient(database.dsn, embeddings_client=embeddings_client).search(
        "keyword query",
        source="slack",
        hybrid=False,
    )

    assert result["search_mode"] == "keyword"
    assert embeddings_client.embeddings.calls == []


def test_search_fuses_keyword_and_vector_results_through_iron_proxy(database, monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    database.add_document(
        "slack:keyword-only",
        title="Launch checklist",
        body="The launch checklist covers every launch checklist item.",
    )
    database.add_document(
        "slack:both",
        title="Weekly notes",
        body="Notes mention the launch once.",
        embedding=embedding(1.0),
    )
    database.add_document(
        "slack:vector-only",
        title="Rollout",
        body="Unrelated words.",
        embedding=embedding(0.9, 0.1),
    )
    database.add_granola_note(
        "not_123", title="Sync", body="Talked.", embedding=embedding(0.8, 0.6)
    )
    database.add_google_doc(
        "google_docs:doc-123:0",
        file_id="doc-123",
        title="Plan",
        body="Words.",
        embedding=embedding(0.6, 0.8),
    )
    embeddings_client = _FakeOpenAIClient(vector=embedding(1.0))

    result = CompanyContextClient(database.dsn, embeddings_client=embeddings_client).search(
        "launch checklist",
        limit=5,
    )

    assert "vector_error" not in result
    assert result["search_mode"] == "hybrid"
    assert result["vector_count"] == 4
    assert _ids(result) == [
        "slack:both",
        "slack:keyword-only",
        "slack:vector-only",
        "granola:note:not_123",
        "google_docs:doc-123:0",
    ]
    both, keyword_only, vector_only, granola, google_doc = result["results"]
    assert both["lane"] == "hybrid"
    assert both["matched_lanes"] == ["keyword", "vector"]
    assert both["keyword_rank"] == 2
    assert both["vector_rank"] == 1
    assert both["vector_similarity"] == pytest.approx(1.0)
    assert keyword_only["lane"] == "keyword"
    assert vector_only["lane"] == "vector"
    assert granola["vector_similarity"] == pytest.approx(0.8)
    assert google_doc["vector_similarity"] == pytest.approx(0.6)
    assert embeddings_client.embeddings.calls == [
        {
            "model": EMBEDDINGS_MODEL,
            "input": "launch checklist",
            "dimensions": 1536,
            "encoding_format": "float",
        }
    ]


def test_search_ignores_embeddings_from_another_model(database, monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_MODEL", "text-embedding-3-large")
    database.add_document("slack:keyword", body="semantic query", embedding=embedding(1.0))
    embeddings_client = _FakeOpenAIClient()

    result = CompanyContextClient(database.dsn, embeddings_client=embeddings_client).search(
        "semantic query",
        source="slack",
    )

    assert "vector_error" not in result
    assert result["search_mode"] == "keyword"
    assert result["vector_count"] == 0
    assert result["results"][0]["lane"] == "indexed"
    assert embeddings_client.embeddings.calls[0]["model"] == "text-embedding-3-large"


def test_search_reports_query_embedding_failure(database, monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    database.add_document("slack:keyword", body="semantic query", embedding=embedding(1.0))
    embeddings_client = _FakeOpenAIClient(error=RuntimeError("embedding unavailable"))

    result = CompanyContextClient(database.dsn, embeddings_client=embeddings_client).search(
        "semantic query",
        source="slack",
    )

    assert result["search_mode"] == "keyword"
    assert result["vector_error"] == "embedding unavailable"
    assert _ids(result) == ["slack:keyword"]


def test_search_reports_incompatible_vector_query(database, monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    database.add_document("slack:keyword", body="semantic query", embedding=embedding(1.0))
    embeddings_client = _FakeOpenAIClient(vector=[0.1, 0.2, 0.3])

    result = CompanyContextClient(database.dsn, embeddings_client=embeddings_client).search(
        "semantic query",
        source="slack",
    )

    assert result["search_mode"] == "keyword"
    assert "dimensions" in result["vector_error"]
    assert _ids(result) == ["slack:keyword"]


def test_search_emits_grouped_lookup_metrics(database, monkeypatch):
    database.add_document("slack:thread:1", body="Shopify launch details", occurred_at=_at(8))
    database.add_document("slack:thread:2", body="More Shopify launch", occurred_at=_at(8, 13))
    database.add_google_doc("google_docs:plan:0", file_id="plan", body="Shopify launch plan")
    pushed_lines = []
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    result = CompanyContextClient(database.dsn).search("Shopify launch", limit=10)

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
            'company_context_lookup_results{lane="indexed",source="docs",'
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


def test_search_emits_zero_result_lookup_metric(database, monkeypatch):
    pushed_lines = []
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    result = CompanyContextClient(database.dsn).search(
        "missing launch",
        source="slack",
        source_type="slack_thread",
        occurred_after="2026-05-01",
    )

    assert result["indexed_count"] == 0
    assert any(
        line.startswith(
            'company_context_lookup_zero_results{requested_source="slack",'
            'requested_source_type="slack_thread",status="ok",time_window="true"} 1 '
        )
        for line in pushed_lines
    )


def test_search_emits_error_lookup_metric(monkeypatch):
    pushed_lines = []
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    result = CompanyContextClient(UNREACHABLE_DSN).search("Shopify launch", source="docs")

    assert result["status"] == "error"
    assert any(
        line.startswith(
            'company_context_lookup_requests{requested_source="docs",'
            'requested_source_type="all",status="error",time_window="false"} 1 '
        )
        for line in pushed_lines
    )


def test_lookup_metrics_use_metrics_runtime_labels(monkeypatch):
    monkeypatch.setenv("METRICS_ENVIRONMENT", "staging")
    monkeypatch.setenv("METRICS_NAMESPACE", "stg-centaur-system")
    pushed_lines = []
    monkeypatch.setattr(
        company_context_client,
        "_push_company_context_lookup_metric_lines",
        lambda lines: pushed_lines.extend(lines),
    )

    company_context_client._emit_company_context_lookup_metrics(
        status="ok",
        requested_source="slack",
        requested_source_type=None,
        occurred_after=None,
        occurred_before=None,
        results=[
            {
                "source": "slack",
                "source_type": "slack_thread",
                "lane": "indexed",
            }
        ],
    )

    assert any(
        line.startswith(
            'company_context_lookup_requests{environment="staging",namespace="stg-centaur-system",'
            'requested_source="slack",requested_source_type="all",status="ok",'
            'time_window="false"} 1 '
        )
        for line in pushed_lines
    )
    assert any(
        line.startswith(
            'company_context_lookup_results{environment="staging",lane="indexed",'
            'namespace="stg-centaur-system",source="slack",source_type="slack_thread"} 1 '
        )
        for line in pushed_lines
    )


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


def test_search_dm_conversations_returns_member_conversations(database):
    database.add_slack_conversation("D123", participants={"U_TOM": "Tom"}, last_seen_at=_at(8))
    database.add_slack_conversation("D999", member=False, participants={"U_TOM": "Tom"})

    result = CompanyContextClient(database.dsn).search_dm_conversations(" Tom ", limit=500)

    assert result["query"] == "Tom"
    assert _ids(result) == ["slack_dm_conversation:T_HOME:D123"]
    item = result["results"][0]
    assert item["score"] > 0
    assert {key: item[key] for key in DM_CONVERSATION_FIELDS} == {
        "source": "slack_dm",
        "source_type": "slack_dm_conversation",
        "home_team_id": "T_HOME",
        "conversation_id": "D123",
        "conversation_type": "im",
        "title": "Slack DM: Tom, U_SELF",
        "is_ext_shared": False,
        "last_seen_at": "2026-05-08T12:00:00+00:00",
        "participant_user_ids": ["U_TOM", "U_SELF"],
        "participant_labels": ["Tom", "U_SELF"],
        "participant_count": 2,
        "matched_labels": ["Tom"],
    }


def test_search_dms_returns_compact_results_for_member_conversations(database):
    database.add_slack_conversation("D123")
    database.add_slack_conversation("D456")
    database.add_slack_conversation("D999", member=False, participants={"U_TOM": "Tom"})
    database.add_slack_message(
        "D123",
        "1770000000.000000",
        body="launch plan\nAlpha attachment",
        occurred_at=_at(8),
    )
    database.add_slack_message("D456", "1770000001.000000", body="launch plan elsewhere")
    database.add_slack_message("D999", "1770000002.000000", body="launch plan secret")

    client = CompanyContextClient(database.dsn)
    result = client.search_dms("launch plan", limit=5, conversation_id=" D123 ")

    assert result["conversation_id"] == "D123"
    assert _ids(result) == ["slack_dm:T_HOME:D123:1770000000.000000"]
    item = result["results"][0]
    assert item["score"] > 0
    assert {key: item[key] for key in DM_MESSAGE_FIELDS} == {
        "source": "slack_dm",
        "source_type": "slack_im",
        "source_document_id": "D123",
        "source_chunk_id": "1770000000.000000",
        "parent_document_id": None,
        "title": "Slack DM",
        "url": "https://slack.example/archives/D123/p1770000000000000",
        "author_name": "U_SELF",
        "access_scope": "slack_dm",
        "occurred_at": "2026-05-08T12:00:00+00:00",
        "conversation_id": "D123",
        "conversation_type": "im",
        "message_ts": "1770000000.000000",
        "thread_ts": None,
        "user_id": "U_SELF",
        "bot_id": "",
        "attachment_count": 0,
        "preview": "launch plan Alpha attachment",
        "lane": "indexed",
        "result_type": "slack_im",
    }
    assert sorted(_ids(client.search_dms("launch plan"))) == [
        "slack_dm:T_HOME:D123:1770000000.000000",
        "slack_dm:T_HOME:D456:1770000001.000000",
    ]


def test_private_channel_documents_use_private_access_scope():
    summary = company_context_client._dm_document_summary(
        {
            "document_id": "slack_dm:T_HOME:G123:1770000000.000000",
            "conversation_id": "G123",
            "conversation_type": "private_channel",
            "message_ts": "1770000000.000000",
            "title": "Slack private channel: #leadership",
            "metadata": {"channel_name": "leadership"},
        }
    )

    assert summary["source_type"] == "slack_private_channel"
    assert summary["access_scope"] == "slack_private_channel"


def test_search_dms_applies_occurred_at_filters(database):
    database.add_slack_conversation("D123")
    database.add_slack_message("D123", "1.0", body="planning", occurred_at=_at(2))
    database.add_slack_message("D123", "2.0", body="planning", occurred_at=_at(9))

    result = CompanyContextClient(database.dsn).search_dms(
        "planning",
        limit=4,
        occurred_after="2026-05-01",
        occurred_before="2026-05-08T12:30:00Z",
    )

    assert result["occurred_after"] == "2026-05-01T00:00:00+00:00"
    assert result["occurred_before"] == "2026-05-08T12:30:00+00:00"
    assert _ids(result) == ["slack_dm:T_HOME:D123:1.0"]


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


def test_list_documents_returns_date_bounded_document_summaries(database):
    database.add_document(
        "slack:thread:later",
        title="Planning sync",
        body="Planning sync with roadmap notes.",
        url="https://slack.example/later",
        author_name="alice",
        occurred_at=_at(6, 15),
        source_updated_at=_at(6, 15, 30),
    )
    database.add_document("slack:thread:earlier", occurred_at=_at(2))
    database.add_document("slack:thread:outside", occurred_at=_at(9))
    database.add_document("slack:day", source_type="slack_channel_day", occurred_at=_at(3))

    result = CompanyContextClient(database.dsn).list_documents(
        limit=2,
        source="slack",
        source_type="slack_thread",
        occurred_after="2026-05-01",
        occurred_before="2026-05-08",
    )

    assert result["occurred_after"] == "2026-05-01T00:00:00+00:00"
    assert result["occurred_before"] == "2026-05-08T00:00:00+00:00"
    assert _ids(result) == ["slack:thread:earlier", "slack:thread:later"]
    assert result["results"][1] == {
        "document_id": "slack:thread:later",
        "source": "slack",
        "source_type": "slack_thread",
        "source_document_id": "slack:thread:later",
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": "Planning sync",
        "url": "https://slack.example/later",
        "author_name": "alice",
        "access_scope": "company",
        "occurred_at": "2026-05-06T15:00:00+00:00",
        "source_updated_at": "2026-05-06T15:30:00+00:00",
        "metadata": {"channel_id": CHANNEL_ID},
        "preview": "Planning sync with roadmap notes.",
    }


def test_list_documents_merges_sources_by_occurred_at(database):
    database.add_document("slack:thread", occurred_at=_at(3))
    database.add_google_doc(
        "google_docs:doc:0", file_id="doc", created_at=_at(2), modified_at=_at(4)
    )
    database.add_granola_note("not_123", occurred_at=_at(5))

    assert _ids(CompanyContextClient(database.dsn).list_documents()) == [
        "google_docs:doc:0",
        "slack:thread",
        "granola:note:not_123",
    ]


def test_latest_date_returns_latest_indexed_slack_timestamp(database):
    database.add_document(
        "slack:thread:1",
        occurred_at=_at(10, 14),
        source_updated_at=_at(10, 15, 30),
    )
    database.add_document("slack:thread:2", occurred_at=_at(9), source_updated_at=_at(9))
    database.add_document(
        "slack:day",
        source_type="slack_channel_day",
        occurred_at=_at(11),
        source_updated_at=_at(11),
    )

    result = CompanyContextClient(database.dsn).latest_date(
        source="slack",
        source_type="slack_thread",
    )

    assert result == {
        "status": "ok",
        "source": "slack",
        "source_type": "slack_thread",
        "document_count": 2,
        "latest_date": "2026-05-10T15:30:00+00:00",
        "latest_source_updated_at": "2026-05-10T15:30:00+00:00",
        "latest_occurred_at": "2026-05-10T14:00:00+00:00",
    }


def test_latest_date_reports_empty_index(database):
    result = CompanyContextClient(database.dsn).latest_date(source="slack")

    assert result == {
        "status": "ok",
        "source": "slack",
        "source_type": None,
        "document_count": 0,
        "latest_date": None,
        "latest_source_updated_at": None,
        "latest_occurred_at": None,
    }


def test_latest_date_counts_slack_dm_messages_and_conversations(database):
    database.add_slack_conversation("D123", last_seen_at=_at(10, 10))
    database.add_slack_message("D123", "1.0", body="hi", occurred_at=_at(8, 14))

    result = CompanyContextClient(database.dsn).latest_date(source="slack_dm")

    assert result["document_count"] == 2
    assert result["latest_occurred_at"] == "2026-05-10T10:00:00+00:00"


@pytest.mark.parametrize(
    ("source_type", "conversation_id"),
    [("slack_im", "D123"), ("slack_private_channel", "G123")],
)
def test_latest_date_filters_slack_dm_messages_by_conversation_type(
    database, source_type, conversation_id
):
    database.add_slack_conversation("D123")
    database.add_slack_message("D123", "1.0", body="hi", occurred_at=_at(8))
    database.add_slack_conversation("G123", conversation_type="private_channel")
    database.add_slack_message("G123", "2.0", body="hi", occurred_at=_at(9))

    result = CompanyContextClient(database.dsn).latest_date(
        source="slack_dm",
        source_type=source_type,
    )

    assert result["document_count"] == 1
    assert (
        result["latest_occurred_at"]
        == (_at(8) if conversation_id == "D123" else _at(9)).isoformat()
    )


def test_read_document_returns_full_content_by_default(database):
    body = "x" * 2_500
    database.add_document(
        "slack:channel_day:C_HOME:2026-05-08",
        source_type="slack_channel_day",
        title="#eng-ai - 2026-05-08",
        body=body,
        metadata={"channel_name": "eng-ai"},
    )

    result = CompanyContextClient(database.dsn).read_document(
        " slack:channel_day:C_HOME:2026-05-08 ",
    )

    assert result["status"] == "ok"
    assert result["document_id"] == "slack:channel_day:C_HOME:2026-05-08"
    assert result["chars"] == 2_500
    assert result["total_chars"] == 2_500
    assert result["truncated"] is False
    assert result["content"] == body
    assert result["metadata"] == {"channel_id": CHANNEL_ID, "channel_name": "eng-ai"}


def test_read_document_can_return_bounded_content(database):
    database.add_document("slack:channel_day:C_HOME:2026-05-08", body="x" * 2_500)

    result = CompanyContextClient(database.dsn).read_document(
        "slack:channel_day:C_HOME:2026-05-08",
        max_chars=1_200,
    )

    assert result["status"] == "ok"
    assert result["chars"] == 1_200
    assert result["total_chars"] == 2_500
    assert result["truncated"] is True
    assert result["content"] == "x" * 1_200


def test_read_document_falls_back_to_oauth_google_docs_index(database):
    body = "OAuth Google Doc content"
    database.add_google_doc("google_docs:doc-123:0", file_id="doc-123", body=body)

    result = CompanyContextClient(database.dsn).read_document(
        "google_docs:doc-123:0",
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


def test_read_document_falls_back_to_granola_note_projection(database):
    body = "Granola note content"
    database.add_granola_note("not_123", title="Launch review", body=body)

    result = CompanyContextClient(database.dsn).read_document(
        "granola:note:not_123",
        max_chars=7,
    )

    assert result["status"] == "ok"
    assert result["source"] == "granola"
    assert result["source_type"] == "granola_note"
    assert result["source_document_id"] == "not_123"
    assert result["author_name"] == "Alice"
    assert result["content"] == "Granola"
    assert result["chars"] == 7
    assert result["total_chars"] == len(body)
    assert result["truncated"] is True


def test_read_document_reports_missing_and_hidden_documents(database):
    database.add_google_doc(
        "google_docs:doc-other:0",
        file_id="doc-other",
        body="Someone else's doc",
        subject="subject-other",
    )
    client = CompanyContextClient(database.dsn)

    assert client.read_document("missing-doc") == {
        "status": "error",
        "error": "document not found: missing-doc",
    }
    assert client.read_document("google_docs:doc-other:0") == {
        "status": "error",
        "error": "document not found: google_docs:doc-other:0",
    }


def test_embeddings_dimensions_defaults_when_unset(monkeypatch):
    monkeypatch.delenv("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS", raising=False)
    assert (
        CompanyContextClient._embeddings_dimensions()
        == company_context_client.DEFAULT_EMBEDDINGS_DIMENSIONS
    )


def test_embeddings_dimensions_reads_the_configured_width(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS", "1024")
    assert CompanyContextClient._embeddings_dimensions() == 1024


def test_embeddings_dimensions_ignores_a_blank_value(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS", "   ")
    assert (
        CompanyContextClient._embeddings_dimensions()
        == company_context_client.DEFAULT_EMBEDDINGS_DIMENSIONS
    )


def test_embeddings_dimensions_rejects_a_non_integer(monkeypatch):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS", "wide")
    with pytest.raises(RuntimeError, match="must be an integer"):
        CompanyContextClient._embeddings_dimensions()


# Above 2000 pgvector stores the vector but cannot index it, so the search this
# tool exists to serve would silently fall back to a sequential scan.
@pytest.mark.parametrize("value", ["0", "-1", "2001"])
def test_embeddings_dimensions_rejects_unindexable_widths(monkeypatch, value):
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS", value)
    with pytest.raises(RuntimeError, match="must be between 1 and 2000"):
        CompanyContextClient._embeddings_dimensions()
