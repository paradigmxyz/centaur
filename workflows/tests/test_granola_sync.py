from __future__ import annotations

import asyncio
import datetime as dt
import importlib
import json
import sys
import types
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[2]))


def _install_workflow_stubs() -> None:
    api_module = sys.modules.get("api") or types.ModuleType("api")
    runtime_control = sys.modules.get("api.runtime_control") or types.ModuleType(
        "api.runtime_control"
    )
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object

    slack_shared = types.ModuleType("workflows.slack.shared")
    slack_shared.env_flag_enabled = lambda _name, default=True: default
    slack_shared.positive_int = lambda value, default: (
        int(value) if value is not None and int(value) > 0 else default
    )

    api_module.runtime_control = runtime_control
    api_module.workflow_engine = workflow_engine
    sys.modules.setdefault("api", api_module)
    sys.modules["api.runtime_control"] = runtime_control
    sys.modules["api.workflow_engine"] = workflow_engine
    sys.modules["workflows.etl_metrics"] = etl_metrics
    sys.modules["workflows.slack.shared"] = slack_shared


def _load(name: str):
    _install_workflow_stubs()
    return importlib.import_module(name)


def test_granola_transcript_text_uses_speaker_identity():
    granola = _load("workflows.granola_sync")

    text = granola._transcript_text(
        [
            {"speaker": {"name": "Alice"}, "text": "Hello"},
            {"speaker": {"email": "bob@example.com"}, "text": "Ship it"},
            {"speaker": {}, "text": ""},
        ]
    )

    assert text == "Alice: Hello\nbob@example.com: Ship it"


def test_granola_access_emails_include_owner_and_attendees_once():
    granola = _load("workflows.granola_sync")

    emails = granola._access_emails(
        {"email": "Alice@Example.com "},
        [
            {"email": "bob@example.com"},
            {"email": "alice@example.com"},
            {"name": "No Email"},
        ],
    )

    assert emails == ["alice@example.com", "bob@example.com"]


def test_granola_context_document_is_user_scoped_to_owner_and_attendees():
    granola = _load("workflows.granola_sync")
    note = {
        "id": "not_123",
        "title": "Launch review",
        "web_url": "https://app.granola.ai/notes/not_123",
        "owner": {"id": "usr_1", "name": "Alice", "email": "Alice@Example.com"},
        "attendees": [
            {"name": "Bob", "email": "bob@example.com"},
            {"name": "Alice", "email": "alice@example.com"},
        ],
        "summary_markdown": "We agreed to ship.",
        "transcript": [
            {"speaker": {"name": "Alice"}, "text": "Let's ship."},
        ],
        "created_at": "2026-07-01T10:00:00Z",
        "updated_at": "2026-07-01T10:30:00Z",
    }
    owner = granola._json_object(note["owner"])
    attendees = granola._json_array(note["attendees"])
    transcript = granola._json_array(note["transcript"])
    access_emails = granola._access_emails(owner, attendees)

    document = granola._granola_context_document(
        note=note,
        note_id="not_123",
        title="Launch review",
        owner=owner,
        attendees=attendees,
        access_emails=access_emails,
        calendar_event={},
        transcript=transcript,
        transcript_text=granola._transcript_text(transcript),
        summary_markdown="We agreed to ship.",
        summary_text="",
        source_created_at=dt.datetime(2026, 7, 1, 10, tzinfo=dt.UTC),
        source_updated_at=dt.datetime(2026, 7, 1, 10, 30, tzinfo=dt.UTC),
    )

    assert document["document_id"] == "granola:note:not_123"
    assert document["note_id"] == "not_123"
    assert document["url"] == "https://app.granola.ai/notes/not_123"
    assert document["access_emails"] == ["alice@example.com", "bob@example.com"]
    assert document["attendee_labels"] == [
        "Bob <bob@example.com>",
        "Alice <alice@example.com>",
    ]
    assert "## Summary\nWe agreed to ship." in document["body"]
    assert "## Transcript\nAlice: Let's ship." in document["body"]
    assert document["metadata"]["access_emails"] == [
        "alice@example.com",
        "bob@example.com",
    ]
    assert document["metadata"]["has_transcript"] is True


def test_granola_checkpoint_metrics_use_workspace_checkpoint_health(monkeypatch):
    granola = _load("workflows.granola_sync")
    calls: dict[str, list[tuple]] = {
        "active": [],
        "failed": [],
        "freshness": [],
    }
    monkeypatch.setattr(
        granola,
        "set_etl_active_scopes",
        lambda *args: calls["active"].append(args),
    )
    monkeypatch.setattr(
        granola,
        "set_etl_failed_scopes",
        lambda *args: calls["failed"].append(args),
    )
    monkeypatch.setattr(
        granola,
        "set_etl_scope_sync_freshness_seconds",
        lambda *args: calls["freshness"].append(args),
    )

    class FakePool:
        def __init__(self) -> None:
            self.fetchrow_calls: list[tuple[str, tuple]] = []

        async def fetchrow(self, query, *args):
            self.fetchrow_calls.append((query, args))
            return {
                "active_scopes": 1,
                "failed_scopes": 1,
                "freshness_seconds": 123.5,
            }

    pool = FakePool()
    asyncio.run(granola._emit_checkpoint_metrics(pool))

    assert len(pool.fetchrow_calls) == 1
    assert "FROM granola_sync_checkpoints" in pool.fetchrow_calls[0][0]
    assert calls == {
        "active": [("granola", 1)],
        "failed": [("granola", 1)],
        "freshness": [("granola", 123.5)],
    }


def test_granola_sync_emits_checkpoint_metrics_after_a_failed_attempt(monkeypatch):
    granola = _load("workflows.granola_sync")
    monkeypatch.setattr(granola, "env_flag_enabled", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(granola, "_client", lambda _ctx: object())

    async def noop(*_args, **_kwargs):
        return None

    async def fail_sync(*_args, **_kwargs):
        raise RuntimeError("Granola API unavailable")

    emitted: list[object] = []

    async def record_metrics(pool):
        emitted.append(pool)

    monkeypatch.setattr(granola, "_record_run_start", noop)
    monkeypatch.setattr(granola, "_load_checkpoint", noop)
    monkeypatch.setattr(granola, "_sync_workspace", fail_sync)
    monkeypatch.setattr(granola, "_update_checkpoint_failure", noop)
    monkeypatch.setattr(granola, "_record_run_finish", noop)
    monkeypatch.setattr(granola, "_emit_checkpoint_metrics", record_metrics)

    pool = object()
    context = types.SimpleNamespace(
        run_id="run-123", _pool=pool, log=lambda *_args, **_kwargs: None
    )

    result = asyncio.run(granola.handler(granola.Input(), context))

    assert result["status"] == "failed"
    assert emitted == [pool]
