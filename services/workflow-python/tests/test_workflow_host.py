from __future__ import annotations

import asyncio
import importlib.util
import os
import sys
import types
import unittest
from pathlib import Path
from unittest.mock import patch


def load_workflow_host():
    module_path = Path(__file__).resolve().parents[1] / "workflow_host.py"
    sys.path.insert(0, str(module_path.parent))
    spec = importlib.util.spec_from_file_location("workflow_host_under_test", module_path)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class FakePool:
    def __init__(self) -> None:
        self.closed = False

    async def close(self) -> None:
        self.closed = True


class FakeRpc:
    def __init__(self) -> None:
        self.drained = False

    async def drain_notifications(self) -> None:
        self.drained = True


class RequestRpc(FakeRpc):
    def __init__(self) -> None:
        super().__init__()
        self.requests = []

    async def request(self, payload):
        self.requests.append(payload)
        message_type = payload["type"]
        if message_type == "ctx.step.get":
            return {"done": False, "checkpoint_name": "checkpoint-1"}
        if message_type == "ctx.step.put":
            return payload["value"]
        if message_type == "ctx.call_tool":
            return {
                "tool": payload["tool"],
                "method": payload["method"],
                "args": payload["args"],
                "via": "rpc",
            }
        if message_type == "ctx.agent_turn":
            return payload["args"]
        if message_type == "ctx.sleep":
            return {"slept": True}
        raise AssertionError(f"unexpected request {payload}")


class WorkflowHostTests(unittest.TestCase):
    def test_workflow_api_modules_are_importable(self) -> None:
        load_workflow_host()

        from api.runtime_control import ControlPlaneError, canonical_json, decode_jsonb
        from api.workflow_engine import Delivery, WorkflowContext

        self.assertEqual(canonical_json({"b": 1, "a": 2}), '{"a":2,"b":1}')
        self.assertEqual(decode_jsonb('{"ok": true}', {}), {"ok": True})
        self.assertEqual(Delivery().metadata, {})
        self.assertTrue(WorkflowContext)

        error = ControlPlaneError("INVALID", "bad input", 422)
        self.assertEqual(error.to_dict()["status_code"], 422)
        self.assertIn("INVALID", str(error))

    def test_step_accepts_step_kind_and_binds_tool_manager_rpc(self) -> None:
        host = load_workflow_host()
        from api import app as workflow_app

        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        async def run_step():
            async def call_tool():
                manager = workflow_app.get_tool_manager()
                return await manager.call_tool_raw("demo", "method", {"x": 1})

            return await ctx.step("call_tool", call_tool, step_kind="tool_call")

        with patch.object(workflow_app, "resolve_tool_shim", return_value=None):
            result = asyncio.run(run_step())

        self.assertEqual(result["via"], "rpc")
        self.assertEqual(rpc.requests[0]["type"], "ctx.step.get")
        self.assertEqual(rpc.requests[0]["step_kind"], "tool_call")
        self.assertEqual(rpc.requests[-1]["type"], "ctx.step.put")
        self.assertEqual(rpc.requests[-1]["step_kind"], "tool_call")

    def test_sleep_sends_duration_seconds(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        asyncio.run(ctx.sleep("pause", 2.5))

        self.assertEqual(
            rpc.requests,
            [{"type": "ctx.sleep", "step": "pause", "duration_seconds": 2.5}],
        )

    def test_tools_proxy_calls_tool_manager(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        async def call_tool():
            return await ctx.tools.demo.method(x=1)

        from api import app as workflow_app

        with patch.object(workflow_app, "resolve_tool_shim", return_value=None):
            result = asyncio.run(call_tool())

        self.assertEqual(
            result,
            {"tool": "demo", "method": "method", "args": {"x": 1}, "via": "rpc"},
        )

    def test_run_agent_accepts_positional_step_name_with_text(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        result = asyncio.run(ctx.run_agent("draft_summary", text="summarize this"))

        self.assertEqual(result, {"name": "draft_summary", "text": "summarize this"})

    def test_create_pool_retries_transient_connection_failure(self) -> None:
        host = load_workflow_host()
        calls = []
        sleeps = []
        pool = FakePool()

        async def create_pool(database_url):
            calls.append(database_url)
            if len(calls) < 3:
                raise ConnectionRefusedError("postgres is still starting")
            return pool

        async def sleep(delay):
            sleeps.append(delay)

        fake_asyncpg = types.SimpleNamespace(create_pool=create_pool)

        with (
            patch.dict(os.environ, {"DATABASE_URL": "postgresql://example/db"}, clear=False),
            patch.dict(sys.modules, {"asyncpg": fake_asyncpg}),
            patch.object(host.asyncio, "sleep", sleep),
        ):
            result = asyncio.run(host.create_pool())

        self.assertIs(result, pool)
        self.assertEqual(calls, ["postgresql://example/db"] * 3)
        self.assertEqual(sleeps, [0.25, 0.5])

    def test_workflow_result_includes_grouping_identifiers(self) -> None:
        host = load_workflow_host()
        pool = FakePool()
        rpc = FakeRpc()

        async def handler(inp, ctx):
            self.assertEqual(inp, {"input": "value"})
            return {"ok": True, "seen_run_id": ctx.run_id}

        registered = host.RegisteredWorkflow(
            workflow_name="sample_workflow",
            source_path="workflows/sample.py",
            handler=handler,
            input_cls=None,
            webhooks=None,
            schedule=None,
        )

        async def create_pool():
            return pool

        with (
            patch.object(
                host,
                "discover_workflows",
                return_value={"sample_workflow": registered},
            ),
            patch.object(host, "create_pool", create_pool),
        ):
            payload = asyncio.run(
                host.run_workflow(
                    {
                        "type": "workflow.start",
                        "workflow_name": "sample_workflow",
                        "run_id": "run-123",
                        "task_id": "task-456",
                        "input": {"input": "value"},
                    },
                    rpc,
                )
            )

        self.assertEqual(
            payload,
            {
                "type": "workflow.result",
                "workflow_run_id": "run-123",
                "run_id": "run-123",
                "workflow_task_id": "task-456",
                "task_id": "task-456",
                "workflow_name": "sample_workflow",
                "result": {"ok": True, "seen_run_id": "run-123"},
            },
        )
        self.assertTrue(rpc.drained)
        self.assertTrue(pool.closed)


if __name__ == "__main__":
    unittest.main()
