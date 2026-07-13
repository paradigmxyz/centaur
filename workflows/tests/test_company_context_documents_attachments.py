from __future__ import annotations

import asyncio
import datetime as dt
import importlib
import json
import sys
import types
from pathlib import Path


def _load_projection_module():
    repo_root = Path(__file__).resolve().parents[2]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = sys.modules.get("api") or types.ModuleType("api")
    runtime_control = sys.modules.get("api.runtime_control") or types.ModuleType(
        "api.runtime_control"
    )
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    runtime_control.decode_jsonb = lambda value, default: (
        value if value is not None else default
    )

    company_context_metrics = types.ModuleType("workflows.company_context_metrics")
    for name in (
        "record_company_context_documents_changed",
        "set_company_context_projection_lag",
    ):
        setattr(company_context_metrics, name, lambda *_args, **_kwargs: None)

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object

    api_module.runtime_control = runtime_control
    api_module.workflow_engine = workflow_engine
    sys.modules.setdefault("api", api_module)
    sys.modules.setdefault("api.runtime_control", runtime_control)
    sys.modules["workflows.company_context_metrics"] = company_context_metrics
    sys.modules["workflows.etl_metrics"] = etl_metrics
    sys.modules.setdefault("api.workflow_engine", workflow_engine)

    return importlib.import_module("workflows.company_context_documents")


projection = _load_projection_module()


class FakeScopeMetricsPool:
    def __init__(self) -> None:
        self.fetchrow_calls: list[str] = []

    async def fetchrow(self, sql):
        self.fetchrow_calls.append(sql)
        return {
            "active_scopes": 7,
            "failed_scopes": 1,
            "freshness_seconds": 42.0,
        }


class FakeWatermarkPool:
    def __init__(self) -> None:
        self.query = ""
        self.args: tuple = ()

    async def fetchrow(self, query, *args):
        self.query = query
        self.args = args
        return {
            "completed_payload": {
                "steps": ["python_host"],
                "output": {
                    "status": "completed",
                    "watermark": "2026-06-18T22:59:36+00:00",
                },
                "workflow_name": "company_context_documents",
            }
        }


class FakeChangedRowsPool:
    def __init__(self) -> None:
        self.fetch_calls: list[tuple[str, tuple]] = []
        self.fetchrow_calls: list[tuple[str, tuple]] = []

    async def fetch(self, query, *args):
        self.fetch_calls.append((query, args))
        return []

    async def fetchrow(self, query, *args):
        self.fetchrow_calls.append((query, args))
        return {
            "changed_messages": 0,
            "changed_attachments": 0,
            "max_updated_at": None,
        }


class FakeWorkflowContext:
    def __init__(self) -> None:
        self.run_id = "run_123"
        self._pool = object()
        self.logs: list[tuple[str, dict]] = []
        self.children: list[tuple[str, dict, str | None]] = []

    def log(self, message, **fields):
        self.logs.append((message, fields))

    async def start_workflow(self, workflow_name, input, *, idempotency_key=None):
        self.children.append((workflow_name, input, idempotency_key))
        return {"task_id": f"task_{len(self.children)}", "created": True}


def test_latest_successful_watermark_reads_absurd_etl_queue():
    pool = FakeWatermarkPool()

    watermark = asyncio.run(
        projection._latest_successful_watermark(
            pool,
            "4b2eb33c-6377-4b1a-97f0-ec28e4427eb5",
        )
    )

    assert watermark == dt.datetime(2026, 6, 18, 22, 59, 36, tzinfo=dt.UTC)
    assert "absurd.t_centaur_workflows_etl" in pool.query
    assert "absurd.r_centaur_workflows_etl" in pool.query
    assert "workflow_runs" not in pool.query
    assert "t.completed_payload" in pool.query
    assert "t.params->>'workflow_name' = $1" in pool.query
    assert "r.run_id::text <> $2" in pool.query
    assert pool.args == (
        "company_context_documents",
        "4b2eb33c-6377-4b1a-97f0-ec28e4427eb5",
    )


def test_load_changed_message_keys_applies_upper_batch_bound():
    pool = FakeChangedRowsPool()
    since = dt.datetime(2026, 6, 18, 22, 58, 36, tzinfo=dt.UTC)
    until = dt.datetime(2026, 6, 19, 4, 58, 36, tzinfo=dt.UTC)

    result = asyncio.run(projection._load_changed_message_keys(pool, since, until))

    assert result["changed_messages"] == 0
    assert pool.fetch_calls
    assert pool.fetchrow_calls
    queries = [query for query, _args in (*pool.fetch_calls, *pool.fetchrow_calls)]
    assert any("updated_at > $1" in query for query in queries)
    assert any("updated_at <= $2" in query for query in queries)
    assert any("a.updated_at > $1" in query for query in queries)
    assert any("a.updated_at <= $2" in query for query in queries)
    for _query, args in (*pool.fetch_calls, *pool.fetchrow_calls):
        assert args == (since, until)


def test_coordinator_starts_one_child_per_enabled_scope(monkeypatch):
    last_watermark = dt.datetime(2026, 6, 18, 22, 59, 36, tzinfo=dt.UTC)
    claimed_scopes: list[str] = []

    async def latest_watermark(_pool, _run_id):
        return last_watermark

    async def claim_scope(_pool, *, scope, **_kwargs):
        claimed_scopes.append(scope)
        return {
            "scope": scope,
            "lease_token": f"lease_{scope}",
            "window_end": dt.datetime(2026, 6, 18, 23, 58, 36, tzinfo=dt.UTC),
        }

    async def noop_async(*_args, **_kwargs):
        return None

    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "false")
    monkeypatch.setenv("LINEAR_ETL_ENABLED", "false")
    monkeypatch.setenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", "true")
    monkeypatch.setattr(projection, "_latest_successful_watermark", latest_watermark)
    monkeypatch.setattr(projection, "_claim_scope", claim_scope)
    monkeypatch.setattr(projection, "_emit_projection_lag_from_checkpoints", noop_async)
    monkeypatch.setattr(projection, "_emit_etl_scope_metrics", noop_async)

    ctx = FakeWorkflowContext()
    result = asyncio.run(
        projection.handler(
            projection.Input(max_window_seconds=3600),
            ctx,
        )
    )

    assert claimed_scopes == ["slack_channel_day", "slack_thread", "slack_attachment"]
    assert [child[1]["scope"] for child in ctx.children] == claimed_scopes
    assert all(child[0] == "company_context_documents" for child in ctx.children)
    assert all("company-context:slack_" in str(child[2]) for child in ctx.children)
    assert result["started_scopes"]
    assert result["batch_size"] == projection.DEFAULT_BATCH_SIZE
    assert ctx.logs[-1][0] == "company_context_documents_coordinator_completed"


def test_scope_batch_advances_cursor_before_starting_one_continuation(monkeypatch):
    window_end = dt.datetime(2026, 6, 18, 23, 58, 36, tzinfo=dt.UTC)
    cursor_at = dt.datetime(2026, 6, 18, 23, 0, tzinfo=dt.UTC)
    advanced: list[tuple] = []

    async def read_owned_scope(_pool, _scope, _lease_token):
        return {
            "window_start": dt.datetime(2026, 6, 18, 22, 58, 36, tzinfo=dt.UTC),
            "window_end": window_end,
            "cursor_updated_at": None,
            "cursor_key": "",
        }

    async def load_scope_page(*_args, **_kwargs):
        return [
            {"projection_updated_at": cursor_at, "projection_key": "doc_a"},
            {"projection_updated_at": cursor_at, "projection_key": "doc_b"},
        ]

    async def project_scope_page(*_args, **_kwargs):
        return 2, 0

    async def advance_scope_cursor(*args):
        advanced.append(args)

    monkeypatch.setattr(projection, "_read_owned_scope", read_owned_scope)
    monkeypatch.setattr(projection, "_load_scope_page", load_scope_page)
    monkeypatch.setattr(projection, "_project_scope_page", project_scope_page)
    monkeypatch.setattr(projection, "_advance_scope_cursor", advance_scope_cursor)
    monkeypatch.setattr(projection, "_batch_size", lambda _value=None: 2)

    ctx = FakeWorkflowContext()
    result = asyncio.run(
        projection._run_scope_batch(
            projection.Input(scope="google_doc", lease_token="lease_1", batch_size=2),
            ctx,
            "google_doc",
        )
    )

    assert advanced == [(ctx._pool, "google_doc", "lease_1", cursor_at, "doc_b")]
    assert ctx.children[0][1]["scope"] == "google_doc"
    assert ctx.children[0][1]["lease_token"] == "lease_1"
    assert result["continuation"]["created"] is True


def test_etl_scope_metrics_no_longer_emit_slack_scope_gauges(monkeypatch):
    calls: list[tuple] = []
    monkeypatch.setattr(
        projection,
        "set_etl_active_scopes",
        lambda *args: calls.append(("active", *args)),
    )
    monkeypatch.setattr(
        projection,
        "set_etl_failed_scopes",
        lambda *args: calls.append(("failed", *args)),
    )
    monkeypatch.setattr(
        projection,
        "set_etl_scope_sync_freshness_seconds",
        lambda *args: calls.append(("freshness", *args)),
    )
    pool = FakeScopeMetricsPool()

    asyncio.run(projection._emit_etl_scope_metrics(pool, ["slack", "google_drive"]))

    assert len(pool.fetchrow_calls) == 1
    assert "google_drive_sync_checkpoints" in pool.fetchrow_calls[0]
    assert "slack_sync_checkpoints" not in pool.fetchrow_calls[0]
    assert calls == [
        ("active", "google_drive", 7),
        ("failed", "google_drive", 1),
        ("freshness", "google_drive", 42.0),
    ]


def test_slack_attachment_document_indexes_metadata_without_private_url():
    row = {
        "channel_id": "C123",
        "channel_name": "eng",
        "message_ts": "1770000000.000100",
        "slack_file_id": "F123",
        "name": "roadmap.pdf",
        "title": "Q3 Roadmap",
        "mimetype": "application/pdf",
        "filetype": "pdf",
        "size_bytes": 12345,
        "permalink": "https://example.slack.com/files/U123/F123/roadmap.pdf",
        "download_status": "downloaded",
        "download_error": "",
        "content_sha256": "abc123",
        "updated_at": dt.datetime(2026, 6, 15, 12, 1, tzinfo=dt.UTC),
        "occurred_at": dt.datetime(2026, 6, 15, 12, 0, tzinfo=dt.UTC),
        "thread_ts": "1770000000.000100",
        "parent_message_ts": None,
        "user_id": "U123",
        "user_name": "alice",
        "real_name": "Alice Example",
        "display_name": "alice",
        "text": "Please review <#C999|product> and <@U456>",
        "message_permalink": "https://example.slack.com/archives/C123/p1770000000000100",
        "url_private": "https://files.slack.com/files-pri/T/F123/roadmap.pdf",
    }

    document = projection._slack_attachment_document(
        row,
        users_by_id={"U456": "bob"},
        channels_by_id={"C999": "product"},
    )

    assert document is not None
    assert document["document_id"] == "slack:attachment:C123:1770000000.000100:F123"
    assert document["source_type"] == "slack_attachment"
    assert document["title"] == "Slack attachment: Q3 Roadmap"
    assert document["url"] == "https://example.slack.com/files/U123/F123/roadmap.pdf"
    assert "- Filename: roadmap.pdf" in document["body"]
    assert "- MIME type: application/pdf" in document["body"]
    assert "- File type: pdf" in document["body"]
    assert "- Content SHA-256: abc123" in document["body"]
    assert "Please review #product and @bob" in document["body"]
    assert "files-pri" not in document["body"]
    assert "url_private" not in document["metadata"]
    assert document["metadata"]["message_permalink"].endswith("p1770000000000100")


def test_attio_meeting_document_indexes_description_and_transcript():
    row = {
        "meeting_id": "mtg_123",
        "title": "Acme renewal call",
        "description": "Customer renewal discussion.",
        "url": "https://app.attio.com/meetings/mtg_123",
        "linked_records": [{"target_object": "companies", "target_record_id": "rec_1"}],
        "participants": [{"name": "Dana"}, {"email": "buyer@example.com"}],
        "organizer_id": "mem_1",
        "organizer_name": "Eli",
        "organizer_email": "eli@example.com",
        "call_recording_ids": ["rec_1"],
        "transcript_text": "Dana: Budget approved\nEli: Next step is legal",
        "transcript_payload": [{"text": "Budget approved"}],
        "content_text": "",
        "content_hash": "",
        "started_at": dt.datetime(2026, 6, 21, 16, 0, tzinfo=dt.UTC),
        "ended_at": dt.datetime(2026, 6, 21, 16, 30, tzinfo=dt.UTC),
        "source_created_at": dt.datetime(2026, 6, 21, 15, 59, tzinfo=dt.UTC),
        "source_updated_at": dt.datetime(2026, 6, 21, 16, 31, tzinfo=dt.UTC),
        "raw_payload": {"id": {"meeting_id": "mtg_123"}},
        "updated_at": dt.datetime(2026, 6, 21, 16, 32, tzinfo=dt.UTC),
    }

    document = projection._attio_meeting_document(row)

    assert document is not None
    assert document["document_id"] == "attio:meeting:mtg_123"
    assert document["source"] == "attio"
    assert document["source_type"] == "attio_meeting"
    assert document["title"] == "Acme renewal call"
    assert "- Organizer: Eli" in document["body"]
    assert "- Participants: Dana, buyer@example.com" in document["body"]
    assert "Customer renewal discussion." in document["body"]
    assert "Dana: Budget approved" in document["body"]
    assert document["author_id"] == "mem_1"
    assert document["metadata"]["has_transcript"] is True
    assert document["metadata"]["meeting_id"] == "mtg_123"
