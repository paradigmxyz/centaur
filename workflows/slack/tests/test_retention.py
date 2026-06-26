from __future__ import annotations

import asyncio
import importlib
import json
import sys
import types
from pathlib import Path


def _load_retention():
    repo_root = Path(__file__).resolve().parents[3]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = types.ModuleType("api")
    runtime_control = types.ModuleType("api.runtime_control")
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    api_module.runtime_control = runtime_control
    sys.modules["api"] = api_module
    sys.modules["api.runtime_control"] = runtime_control

    vm_metrics = types.ModuleType("api.vm_metrics")
    vm_metrics.metric_calls = []

    def record_etl_items_deleted(*args):
        vm_metrics.metric_calls.append(args)

    for name in (
        "record_slack_etl_rate_limit",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(vm_metrics, name, lambda *_args, **_kwargs: None)
    vm_metrics.record_etl_items_deleted = record_etl_items_deleted
    api_module.vm_metrics = vm_metrics
    sys.modules["api.vm_metrics"] = vm_metrics

    workflow_engine = types.ModuleType("api.workflow_engine")

    class WorkflowContext:
        pass

    workflow_engine.WorkflowContext = WorkflowContext
    api_module.workflow_engine = workflow_engine
    sys.modules["api.workflow_engine"] = workflow_engine

    centaur_sdk = types.ModuleType("centaur_sdk")
    centaur_sdk.secret = lambda _name, default=None: default
    sys.modules["centaur_sdk"] = centaur_sdk

    for module_name in ("workflows.slack.shared", "workflows.slack.retention"):
        sys.modules.pop(module_name, None)
    return importlib.import_module("workflows.slack.retention")


class FakePool:
    def __init__(self, values: list[int]) -> None:
        self.values = values
        self.calls: list[tuple[str, tuple[object, ...]]] = []

    async def fetchval(self, sql: str, *args: object) -> int:
        self.calls.append((sql, args))
        return self.values.pop(0)


def test_schedule_disabled_without_positive_ttl(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_RETENTION_DAYS", raising=False)
    monkeypatch.delenv("SLACK_DM_RETENTION_DAYS", raising=False)
    monkeypatch.delenv("SLACK_RETENTION_ENABLED", raising=False)

    retention = _load_retention()

    assert retention.SCHEDULE["enabled"] is False


def test_schedule_uses_minute_interval_and_positive_ttl(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_RETENTION_DAYS", "30")
    monkeypatch.setenv("SLACK_DM_RETENTION_DAYS", "0")
    monkeypatch.setenv("SLACK_RETENTION_INTERVAL_MINUTES", "5")

    retention = _load_retention()

    assert retention.SCHEDULE["enabled"] is True
    assert retention.SCHEDULE["interval_seconds"] == 300


def test_prune_slack_etl_deletes_expected_tables():
    retention = _load_retention()
    pool = FakePool([1, 2, 3, 4])

    counts = asyncio.run(
        retention.prune_slack_etl(pool, retention_days=14, dry_run=False)
    )

    assert counts == {
        "company_context_documents": 1,
        "messages": 2,
        "backfill_jobs": 3,
        "runs": 4,
    }
    assert [args for _sql, args in pool.calls] == [(14,), (14,), (14,), (14,)]
    sql = "\n".join(call[0] for call in pool.calls)
    assert "DELETE FROM company_context_documents" in sql
    assert "source = 'slack'" in sql
    assert "DELETE FROM slack_sync_messages" in sql
    assert "DELETE FROM slack_sync_backfill_jobs" in sql
    assert "status IN ('completed', 'failed')" in sql
    assert "DELETE FROM slack_sync_runs" in sql
    assert "status <> 'running'" in sql


def test_prune_slack_dm_dry_run_counts_expected_tables():
    retention = _load_retention()
    pool = FakePool([5, 6, 7, 8])

    counts = asyncio.run(retention.prune_slack_dm(pool, retention_days=7, dry_run=True))

    assert counts == {
        "messages": 5,
        "conversations": 6,
        "backfill_jobs": 7,
        "runs": 8,
    }
    sql = "\n".join(call[0] for call in pool.calls)
    assert "SELECT COUNT(*) FROM slack_dm_sync_messages" in sql
    assert "SELECT COUNT(*) FROM slack_dm_sync_conversations" in sql
    assert "NOT EXISTS" in sql
    assert "SELECT COUNT(*) FROM slack_dm_sync_backfill_jobs" in sql
    assert "SELECT COUNT(*) FROM slack_dm_sync_runs" in sql
    assert "DELETE FROM" not in sql


def test_handler_records_metrics_for_non_dry_run():
    retention = _load_retention()
    pool = FakePool([1, 0, 2, 0, 0, 3, 0, 4])
    ctx = types.SimpleNamespace(_pool=pool)
    inp = retention.Input(etl_retention_days=10, dm_retention_days=20)

    result = asyncio.run(retention.handler(inp, ctx))

    assert result["slack_etl"]["company_context_documents"] == 1
    assert result["slack_dm"]["conversations"] == 3
    metric_calls = sys.modules["api.vm_metrics"].metric_calls
    assert ("slack", "retention", "company_context_documents", 1) in metric_calls
    assert ("slack", "retention", "backfill_jobs", 2) in metric_calls
    assert ("slack_dm", "retention", "conversations", 3) in metric_calls
    assert ("slack_dm", "retention", "runs", 4) in metric_calls
