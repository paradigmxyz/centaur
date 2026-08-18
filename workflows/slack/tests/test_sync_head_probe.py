"""Busy-channel incremental sync: head probe, watermark monotonicity, enqueue guards.

Regression tests for the deadlock where a channel with more than one page of
backlog froze forever: the oldest-anchored window page kept the watermark at
the backlog's density fixed point while the hourly continuation re-enqueue
clobbered the backfill worker's cursor progress.
"""

from __future__ import annotations

import asyncio
import importlib
import json
import sys
import time
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

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "record_etl_items_enqueued",
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.etl_metrics"] = etl_metrics

    slack_metrics = types.ModuleType("workflows.slack.metrics")
    for name in (
        "observe_slack_retention_run_duration",
        "record_slack_etl_rate_limit",
        "record_slack_retention_api_rate_limited",
        "record_slack_retention_api_request",
        "record_slack_retention_channel_failure",
        "record_slack_retention_failure",
        "record_slack_retention_messages_processed",
        "record_slack_retention_run",
        "set_slack_retention_last_failure_timestamp",
        "set_slack_retention_watermark_lag_seconds",
    ):
        setattr(slack_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.slack.metrics"] = slack_metrics

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


class FakeBusyClient:
    """First call returns an overflowing oldest-anchored window page; the head
    probe (no oldest, lookback 0) returns the live newest page."""

    def __init__(self, *, head_ts: str, window_watermark: str = "1770000100.000001") -> None:
        self.history_calls: list[dict] = []
        self.head_ts = head_ts
        self.window_watermark = window_watermark

    def _etl_access_mode(self):
        return "test"

    def _list_etl_channels(self, *_args, **_kwargs):
        return [{"id": "C123", "name": "busy"}]

    def _list_etl_users(self, *_args, **_kwargs):
        return []

    def _sync_etl_channel_history(self, channel_id, **kwargs):
        self.history_calls.append({"channel_id": channel_id, **kwargs})
        if len(self.history_calls) == 1:
            return {
                "messages": [
                    {
                        "channel_id": channel_id,
                        "timestamp": self.window_watermark,
                        "text": "stale backlog slice",
                    }
                ],
                "has_more": True,
                "next_cursor": "cursor-window",
                "sync_state": {
                    "cursor": "cursor-window",
                    "watermark": self.window_watermark,
                    "oldest": kwargs.get("oldest"),
                    "latest": None,
                },
            }
        return {
            "messages": [
                {
                    "channel_id": channel_id,
                    "timestamp": self.head_ts,
                    "thread_ts": self.head_ts,
                    "reply_count": 2,
                    "text": "live head",
                }
            ],
            "has_more": False,
            "next_cursor": None,
            "sync_state": {
                "cursor": None,
                "watermark": self.head_ts,
                "oldest": None,
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
        "upserted": [],
    }
    fake_client = client

    async def fake_load_checkpoint(_pool, _channel_id):
        return checkpoint

    async def fake_upsert_messages(_pool, rows):
        calls["upserted"].extend(rows)
        return len(rows)

    async def fake_load_thread_refresh_times(*_args, **_kwargs):
        return {}

    async def fake_update_checkpoint_success(_pool, **kwargs):
        calls["checkpoint_success"].append(kwargs)

    async def fake_enqueue_backfill_job(_pool, **kwargs):
        calls["enqueued"].append(kwargs)

    async def fake_widen(_pool, **_kwargs):
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
    monkeypatch.setattr(sync, "record_run_finish", _noop)
    monkeypatch.setattr(sync, "widen_channel_bootstrap_job", fake_widen)

    return calls


def test_overflowing_window_probes_head_and_advances_watermark(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.delenv("SLACK_BACKFILL_ENABLED", raising=False)
    sync = _load_sync()
    head_ts = f"{time.time():.6f}"
    client = FakeBusyClient(head_ts=head_ts)
    calls = _patch_handler_io(
        monkeypatch,
        sync,
        checkpoint={"watermark_ts": "1770000050.000001", "last_error": ""},
        client=client,
    )

    result = asyncio.run(sync.handler(sync.Input(), FakeContext()))

    assert result["status"] == "completed"
    # Window fetch, then the head probe with a default newest-first call.
    assert len(client.history_calls) == 2
    head_call = client.history_calls[1]
    assert head_call["lookback_days"] == 0
    assert "oldest" not in head_call or head_call.get("oldest") is None
    assert head_call["state"] == {
        "cursor": None,
        "watermark": None,
        "oldest": None,
        "latest": None,
    }

    # Both the stale slice and the live head were upserted.
    upserted_ts = {row["message_ts"] for row in calls["upserted"]}
    assert upserted_ts == {"1770000100.000001", head_ts}

    # Watermark jumps to the live head, not the window page max.
    assert calls["checkpoint_success"] == [
        {
            "channel_id": "C123",
            "watermark_ts": head_ts,
            "run_id": "slack_sync_wfr_test",
        }
    ]

    # The continuation carries the window cursor and must not clobber
    # in-flight jobs; the head thread refresh gets the same protection.
    continuations = [
        c
        for c in calls["enqueued"]
        if c["job_type"] == sync.BACKFILL_JOB_CHANNEL_CONTINUATION
    ]
    assert len(continuations) == 1
    assert continuations[0]["payload"]["cursor"] == "cursor-window"
    assert continuations[0]["refresh_pending"] is False
    # One STABLE key per channel for the standing incremental continuation —
    # a window-derived key would mint a new overlapping job every tick now
    # that the watermark advances.
    assert continuations[0]["job_key"] == "continuation:C123:incremental"

    thread_refreshes = [
        c
        for c in calls["enqueued"]
        if c["job_type"] == sync.BACKFILL_JOB_THREAD_REFRESH
    ]
    assert len(thread_refreshes) == 1
    assert thread_refreshes[0]["payload"] == {"thread_ts": head_ts}
    assert thread_refreshes[0]["refresh_pending"] is False


class FakeProbeFailClient(FakeBusyClient):
    """Window page succeeds; the head probe raises (e.g. rate limit)."""

    def _sync_etl_channel_history(self, channel_id, **kwargs):
        if self.history_calls:
            self.history_calls.append({"channel_id": channel_id, **kwargs})
            raise RuntimeError("Slack API error: ratelimited")
        return super()._sync_etl_channel_history(channel_id, **kwargs)


def test_head_probe_failure_keeps_window_progress(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.delenv("SLACK_BACKFILL_ENABLED", raising=False)
    sync = _load_sync()
    client = FakeProbeFailClient(head_ts="unused")
    calls = _patch_handler_io(
        monkeypatch,
        sync,
        checkpoint={"watermark_ts": "1770000050.000001", "last_error": ""},
        client=client,
    )

    ctx = FakeContext()
    result = asyncio.run(sync.handler(sync.Input(), ctx))

    # The probe is best-effort: its failure must not discard the fetched
    # window page, the continuation enqueue, or the checkpoint write.
    assert result["status"] == "completed"
    assert len(client.history_calls) == 2
    assert [row["message_ts"] for row in calls["upserted"]] == ["1770000100.000001"]
    assert calls["checkpoint_success"][0]["watermark_ts"] == "1770000100.000001"
    assert any(
        c["job_type"] == sync.BACKFILL_JOB_CHANNEL_CONTINUATION
        for c in calls["enqueued"]
    )
    assert any(name == "slack_sync_head_probe_failed" for name, _ in ctx.logs)


def test_head_probe_skipped_when_backfill_disabled(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("SLACK_BACKFILL_ENABLED", "false")
    sync = _load_sync()
    client = FakeBusyClient(head_ts="1779999999.000001")
    calls = _patch_handler_io(monkeypatch, sync, client=client)

    result = asyncio.run(sync.handler(sync.Input(), FakeContext()))

    # Without the backfill worker there is nothing to drain the middle of the
    # backlog, so jumping the watermark would certify a permanent hole.
    assert result["status"] == "completed"
    assert len(client.history_calls) == 1
    assert calls["checkpoint_success"][0]["watermark_ts"] == "1770000100.000001"


def test_head_probe_skipped_for_bounded_manual_runs(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.delenv("SLACK_BACKFILL_ENABLED", raising=False)
    sync = _load_sync()
    client = FakeBusyClient(head_ts="1779999999.000001")
    _calls = _patch_handler_io(monkeypatch, sync, client=client)

    result = asyncio.run(
        sync.handler(sync.Input(latest="1770000200.000000"), FakeContext())
    )

    assert result["status"] == "completed"
    # An explicit `latest` bound means a deliberate historical window: no probe.
    assert len(client.history_calls) == 1


class FakeQuietClient(FakeBusyClient):
    """Single page, no backlog, watermark below the stored checkpoint."""

    def _sync_etl_channel_history(self, channel_id, **kwargs):
        self.history_calls.append({"channel_id": channel_id, **kwargs})
        return {
            "messages": [
                {
                    "channel_id": channel_id,
                    "timestamp": self.window_watermark,
                    "text": "old overlap page",
                }
            ],
            "has_more": False,
            "next_cursor": None,
            "sync_state": {
                "cursor": None,
                "watermark": self.window_watermark,
                "oldest": kwargs.get("oldest"),
                "latest": None,
            },
        }


def test_watermark_never_regresses_below_checkpoint(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.delenv("SLACK_BACKFILL_ENABLED", raising=False)
    sync = _load_sync()
    client = FakeQuietClient(
        head_ts="unused", window_watermark="1770000100.000001"
    )
    calls = _patch_handler_io(
        monkeypatch,
        sync,
        checkpoint={"watermark_ts": "1775000000.000100", "last_error": ""},
        client=client,
    )

    result = asyncio.run(sync.handler(sync.Input(), FakeContext()))

    assert result["status"] == "completed"
    assert len(client.history_calls) == 1
    assert calls["checkpoint_success"] == [
        {
            "channel_id": "C123",
            "watermark_ts": "1775000000.000100",
            "run_id": "slack_sync_wfr_test",
        }
    ]


def test_max_slack_ts_ignores_invalid_and_orders_numerically():
    sync = _load_sync()
    assert sync._max_slack_ts(None, "", "not-a-ts") is None
    assert (
        sync._max_slack_ts("1770000000.000100", "1770000000.000099", None)
        == "1770000000.000100"
    )
    # Numeric, not lexicographic: "9.5" > "10" lexicographically but not numerically.
    assert sync._max_slack_ts("9.5", "10.0") == "10.0"
