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
        "observe_company_context_document_size",
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

    def log(self, message, **fields):
        self.logs.append((message, fields))


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


def test_handler_advances_empty_bounded_window(monkeypatch):
    last_watermark = dt.datetime(2026, 6, 18, 22, 59, 36, tzinfo=dt.UTC)
    seen_bounds: dict[str, dt.datetime | None] = {}

    async def latest_watermark(_pool, _run_id):
        return last_watermark

    async def load_slack_lookup_maps(_pool):
        return {}, {}

    async def load_changed_message_keys(_pool, since, until=None):
        seen_bounds["since"] = since
        seen_bounds["until"] = until
        return {
            "channel_days": [],
            "threads": [],
            "attachments": [],
            "changed_messages": 0,
            "changed_attachments": 0,
            "max_updated_at": None,
        }

    async def noop_async(*_args, **_kwargs):
        return None

    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "false")
    monkeypatch.setenv("LINEAR_ETL_ENABLED", "false")
    monkeypatch.setenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", "true")
    monkeypatch.setattr(projection, "_latest_successful_watermark", latest_watermark)
    monkeypatch.setattr(projection, "_load_slack_lookup_maps", load_slack_lookup_maps)
    monkeypatch.setattr(
        projection,
        "_load_changed_message_keys",
        load_changed_message_keys,
    )
    monkeypatch.setattr(
        projection,
        "_emit_company_context_document_size_snapshot",
        noop_async,
    )
    monkeypatch.setattr(projection, "_emit_etl_scope_metrics", noop_async)

    ctx = FakeWorkflowContext()
    result = asyncio.run(
        projection.handler(
            projection.Input(max_window_seconds=3600),
            ctx,
        )
    )

    expected_since = dt.datetime(2026, 6, 18, 22, 58, 36, tzinfo=dt.UTC)
    expected_until = dt.datetime(2026, 6, 18, 23, 58, 36, tzinfo=dt.UTC)
    assert seen_bounds == {"since": expected_since, "until": expected_until}
    assert result["changed_messages"] == 0
    assert result["batch_until"] == expected_until.isoformat()
    assert result["watermark"] == expected_until.isoformat()
    assert result["remaining_lag_seconds"] is not None
    assert ctx.logs[-1][0] == "company_context_documents_completed"


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
