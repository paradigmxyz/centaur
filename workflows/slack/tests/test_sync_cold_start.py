from __future__ import annotations

import asyncio
import importlib
import json
import sys
import types
from pathlib import Path


def _load_sync():
    repo_root = Path(__file__).resolve().parents[3]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = sys.modules.setdefault("api", types.ModuleType("api"))

    runtime_control = types.ModuleType("api.runtime_control")
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    api_module.runtime_control = runtime_control
    sys.modules["api.runtime_control"] = runtime_control

    vm_metrics = types.ModuleType("api.vm_metrics")
    for name in (
        "record_etl_items_enqueued",
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
        "record_slack_etl_rate_limit",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(vm_metrics, name, lambda *_args, **_kwargs: None)
    api_module.vm_metrics = vm_metrics
    sys.modules["api.vm_metrics"] = vm_metrics

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object
    api_module.workflow_engine = workflow_engine
    sys.modules["api.workflow_engine"] = workflow_engine

    centaur_sdk = sys.modules.setdefault("centaur_sdk", types.ModuleType("centaur_sdk"))
    centaur_sdk.secret = lambda _name, default=None: default

    return importlib.import_module("workflows.slack.sync")


class FakeContext:
    run_id = "wfr_test"
    _pool = object()

    def __init__(self) -> None:
        self.logs: list[tuple[str, dict]] = []

    def log(self, name: str, **fields):
        self.logs.append((name, fields))


class FakeClient:
    def __init__(self, *, cursor: str | None = None) -> None:
        self.history_calls: list[dict] = []
        self.cursor = cursor

    def _etl_access_mode(self):
        return "test"

    def _list_etl_channels(self, *_args, **_kwargs):
        return [{"id": "C123", "name": "cold-start"}]

    def _list_etl_users(self, *_args, **_kwargs):
        return []

    def _sync_etl_channel_history(self, channel_id, **kwargs):
        self.history_calls.append({"channel_id": channel_id, **kwargs})
        return {
            "messages": [
                {
                    "channel_id": channel_id,
                    "timestamp": "1770000000.000100",
                    "thread_ts": "1770000000.000100",
                    "text": "hello",
                }
            ],
            "sync_state": {
                "cursor": self.cursor,
                "watermark": "1770000000.000100",
                "oldest": kwargs["oldest"],
                "latest": None,
            },
        }


async def _noop(*_args, **_kwargs):
    return None


async def _zero(*_args, **_kwargs):
    return 0


def _patch_handler_io(monkeypatch, sync, *, checkpoint=None, client=None):
    calls: dict[str, list] = {
        "checkpoint_success": [],
        "enqueued": [],
        "finish": [],
        "widened": [],
    }
    fake_client = client or FakeClient()

    async def fake_load_checkpoint(_pool, _channel_id):
        return checkpoint

    async def fake_upsert_messages(_pool, rows):
        return len(rows)

    async def fake_load_thread_refresh_times(*_args, **_kwargs):
        return {}

    async def fake_update_checkpoint_success(_pool, **kwargs):
        calls["checkpoint_success"].append(kwargs)

    async def fake_enqueue_backfill_job(_pool, **kwargs):
        calls["enqueued"].append(kwargs)

    async def fake_record_run_finish(_pool, **kwargs):
        calls["finish"].append(kwargs)

    async def fake_widen_channel_bootstrap_job(_pool, **kwargs):
        calls["widened"].append(kwargs)
        return False

    monkeypatch.setattr(sync, "_client", lambda: fake_client)
    monkeypatch.setattr(sync, "_upsert_channels", _noop)
    monkeypatch.setattr(sync, "_upsert_users", _zero)
    monkeypatch.setattr(sync, "_load_checkpoint", fake_load_checkpoint)
    monkeypatch.setattr(sync, "_upsert_messages", fake_upsert_messages)
    monkeypatch.setattr(sync, "load_thread_refresh_times", fake_load_thread_refresh_times)
    monkeypatch.setattr(sync, "_update_checkpoint_success", fake_update_checkpoint_success)
    monkeypatch.setattr(sync, "_update_checkpoint_failure", _noop)
    monkeypatch.setattr(sync, "enqueue_backfill_job", fake_enqueue_backfill_job)
    monkeypatch.setattr(sync, "emit_slack_checkpoint_metrics", _noop)
    monkeypatch.setattr(sync, "record_run_start", _noop)
    monkeypatch.setattr(sync, "record_run_finish", fake_record_run_finish)
    monkeypatch.setattr(
        sync,
        "widen_channel_bootstrap_job",
        fake_widen_channel_bootstrap_job,
    )

    return fake_client, calls


def test_cold_start_channel_uses_full_lookback_window(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    sync = _load_sync()
    client, calls = _patch_handler_io(monkeypatch, sync)

    monkeypatch.setattr(sync, "_ts_now_minus_days", lambda days: f"days:{days}")

    result = asyncio.run(sync.handler(sync.Input(), FakeContext()))

    assert result["status"] == "completed"
    assert client.history_calls[0]["oldest"] == "days:30"
    assert calls["checkpoint_success"] == [
        {
            "channel_id": "C123",
            "watermark_ts": "1770000000.000100",
            "run_id": "slack_sync_wfr_test",
        }
    ]
    assert calls["enqueued"] == []
    assert calls["widened"] == []


def test_watermarked_channel_keeps_incremental_overlap(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    sync = _load_sync()
    client, calls = _patch_handler_io(
        monkeypatch,
        sync,
        checkpoint={"watermark_ts": "1771000000.000100", "last_error": ""},
    )

    monkeypatch.setattr(
        sync,
        "_ts_minus_days",
        lambda ts, days: f"minus:{ts}:{days}",
    )
    monkeypatch.setattr(sync, "_ts_now_minus_days", lambda days: f"days:{days}")

    result = asyncio.run(sync.handler(sync.Input(), FakeContext()))

    assert result["status"] == "completed"
    assert client.history_calls[0]["oldest"] == "minus:1771000000.000100:3"
    assert calls["widened"] == [
        {
            "channel_id": "C123",
            "window_oldest": "days:30",
            "lookback_days": 30,
            "thread_lookback_days": 3,
            "run_id": "slack_sync_wfr_test",
            "priority": 150,
        }
    ]
