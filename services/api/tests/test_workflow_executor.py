from __future__ import annotations

import importlib
import sys

import pytest


class _FakePool:
    async def fetchrow(self, *_args, **_kwargs):
        return {
            "run_id": "wfr_test",
            "workflow_name": "slack_thread_turn",
            "input_json": {},
            "status": "running",
            "created_at": None,
            "worker_id": "worker-test",
        }


@pytest.mark.asyncio
async def test_workflow_executor_sets_app_state_db_pool(monkeypatch):
    monkeypatch.setenv("DATABASE_URL", "postgresql://example.invalid/centaur")
    sys.modules.pop("api.workflow_executor", None)

    workflow_executor = importlib.import_module("api.workflow_executor")
    pool = _FakePool()

    async def fake_create_pool(_database_url):
        return pool

    async def fake_close_pool(_pool):
        assert _pool is pool

    async def fake_run_handler(handler_pool, run_row):
        assert handler_pool is pool
        assert run_row["run_id"] == "wfr_test"
        assert workflow_executor.app.state.db_pool is pool

    monkeypatch.setattr(workflow_executor, "create_pool", fake_create_pool)
    monkeypatch.setattr(workflow_executor, "close_pool", fake_close_pool)
    monkeypatch.setattr(workflow_executor, "discover_workflow_handlers", lambda: {})
    monkeypatch.setattr(workflow_executor, "_run_handler", fake_run_handler)

    assert await workflow_executor._run("wfr_test") == 0
