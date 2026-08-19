from __future__ import annotations

import asyncio
import contextlib
import importlib.util
import json
import os
import select
import subprocess
import sys
import tempfile
import types
import unittest
from collections.abc import Iterator
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
        if message_type == "ctx.run_agents":
            return {
                "results": [
                    {
                        "index": index,
                        "name": agent["name"],
                        "ok": True,
                        "result": agent,
                    }
                    for index, agent in enumerate(payload["agents"])
                ],
                "succeeded": len(payload["agents"]),
                "failed": 0,
            }
        if message_type == "ctx.workflow.start":
            return {
                "workflow_name": payload["workflow_name"],
                "task_id": "task-child",
                "run_id": "run-child",
                "created": True,
            }
        if message_type == "ctx.post_to_slack":
            return {"channel": payload["channel"], "ts": "1710000000.000100"}
        if message_type == "ctx.sleep":
            return {"slept": True}
        if message_type == "ctx.event.wait":
            return {"approved": True}
        raise AssertionError(f"unexpected request {payload}")


class WorkflowHostTests(unittest.TestCase):
    @contextlib.contextmanager
    def workflow_host(
        self,
        source: str | None = None,
        *,
        filename: str = "workflow.py",
    ) -> Iterator[subprocess.Popen[str]]:
        host_path = Path(__file__).resolve().parents[1] / "workflow_host.py"
        with tempfile.TemporaryDirectory() as tmp:
            if source is not None:
                (Path(tmp) / filename).write_text(source)
            proc = subprocess.Popen(
                [sys.executable, str(host_path)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env={**os.environ, "WORKFLOW_DIRS": tmp},
            )
            assert proc.stdin is not None
            assert proc.stdout is not None
            assert proc.stderr is not None
            try:
                yield proc
            finally:
                self.stop_host(proc)

    def send_host_message(
        self,
        proc: subprocess.Popen[str],
        message: dict,
    ) -> None:
        assert proc.stdin is not None
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()

    def read_host_message(
        self,
        proc: subprocess.Popen[str],
        *,
        timeout: float = 2,
    ) -> dict:
        assert proc.stdout is not None
        readable, _, _ = select.select([proc.stdout], [], [], timeout)
        self.assertTrue(readable, "workflow host did not emit a response")
        line = proc.stdout.readline()
        self.assertTrue(line, "workflow host closed stdout before emitting a response")
        return json.loads(line)

    def stop_host(self, proc: subprocess.Popen[str]) -> None:
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=2)
        for stream in (proc.stdin, proc.stdout, proc.stderr):
            if stream is not None and not stream.closed:
                stream.close()

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

    def test_wait_for_event_sends_durable_event_identity_and_timeout(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        result = asyncio.run(
            ctx.wait_for_event("approval", "review", "change:42", timeout=30)
        )

        self.assertEqual(result, {"approved": True})
        self.assertEqual(
            rpc.requests,
            [
                {
                    "type": "ctx.event.wait",
                    "step": "approval",
                    "event_type": "review",
                    "correlation_id": "change:42",
                    "timeout_seconds": 30.0,
                }
            ],
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

    def test_agent_turn_applies_workflow_agent_defaults(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
            agent_defaults={"model": "claude-opus-4-8", "reasoning": "high"},
        )

        result = asyncio.run(ctx.agent_turn("do the thing"))

        self.assertEqual(
            result,
            {"model": "claude-opus-4-8", "reasoning": "high", "text": "do the thing"},
        )

    def test_agent_turn_per_call_kwargs_override_agent_defaults(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
            agent_defaults={"model": "claude-opus-4-8", "reasoning": "high"},
        )

        result = asyncio.run(ctx.agent_turn("cheap step", reasoning="low"))

        self.assertEqual(
            result,
            {"model": "claude-opus-4-8", "reasoning": "low", "text": "cheap step"},
        )

    def test_agent_turn_forwards_principal_foreign_id(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        result = asyncio.run(
            ctx.agent_turn("do the thing", principal="finance-automation")
        )

        self.assertEqual(
            result,
            {"principal": "finance-automation", "text": "do the thing"},
        )

    def test_run_agents_applies_defaults_and_preserves_input_order(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
            agent_defaults={"harness": "codex", "reasoning": "high"},
        )

        result = asyncio.run(
            ctx.run_agents(
                [
                    {"name": "correctness", "text": "Review correctness"},
                    {
                        "name": "security",
                        "text": "Review security",
                        "reasoning": "medium",
                    },
                ],
                max_concurrency=2,
            )
        )

        self.assertEqual(rpc.requests[-1]["type"], "ctx.run_agents")
        self.assertEqual(rpc.requests[-1]["max_concurrency"], 2)
        self.assertEqual(
            rpc.requests[-1]["agents"],
            [
                {
                    "harness": "codex",
                    "reasoning": "high",
                    "name": "correctness",
                    "text": "Review correctness",
                },
                {
                    "harness": "codex",
                    "reasoning": "medium",
                    "name": "security",
                    "text": "Review security",
                },
            ],
        )
        self.assertEqual(
            [item["name"] for item in result["results"]],
            ["correctness", "security"],
        )

    def test_run_agents_rejects_non_mapping_items_before_rpc(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        with self.assertRaisesRegex(TypeError, "agent at index 1 must be a dict"):
            asyncio.run(
                ctx.run_agents(
                    [
                        {"name": "correctness", "text": "Review correctness"},
                        "not-an-agent",  # type: ignore[list-item]
                    ]
                )
            )

        self.assertEqual(rpc.requests, [])

    def test_start_workflow_enqueues_durable_child_with_idempotency_key(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        result = asyncio.run(
            ctx.start_workflow(
                "company_context_documents",
                {"scope": "slack_thread"},
                idempotency_key="company-context:slack-thread:42",
            )
        )

        self.assertEqual(result["task_id"], "task-child")
        self.assertEqual(
            rpc.requests,
            [
                {
                    "type": "ctx.workflow.start",
                    "workflow_name": "company_context_documents",
                    "input": {"scope": "slack_thread"},
                    "idempotency_key": "company-context:slack-thread:42",
                }
            ],
        )

    def test_post_to_slack_sends_optional_custom_identity(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()
        ctx = host.WorkflowContext(
            rpc,
            run_id="run-123",
            task_id="task-456",
            workflow_name="sample",
        )

        result = asyncio.run(
            ctx.post_to_slack(
                "U12345678",
                "Your date is approaching.",
                username="The Date Goblin",
                icon_emoji=":female_mage:",
            )
        )

        self.assertEqual(result["channel"], "U12345678")
        self.assertEqual(
            rpc.requests,
            [
                {
                    "type": "ctx.post_to_slack",
                    "channel": "U12345678",
                    "text": "Your date is approaching.",
                    "args": {
                        "username": "The Date Goblin",
                        "icon_emoji": ":female_mage:",
                    },
                }
            ],
        )

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

    def test_run_workflow_threads_agent_defaults_into_context(self) -> None:
        host = load_workflow_host()
        rpc = RequestRpc()

        async def handler(inp, ctx):
            return await ctx.agent_turn("do the thing")

        registered = host.RegisteredWorkflow(
            workflow_name="sample_workflow",
            source_path="workflows/sample.py",
            handler=handler,
            input_cls=None,
            webhooks=None,
            schedule=None,
            agent_defaults={"model": "claude-opus-4-8", "reasoning": "high"},
        )

        async def create_pool():
            return None

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
                        "input": {},
                    },
                    rpc,
                )
            )

        self.assertEqual(
            payload["result"],
            {"model": "claude-opus-4-8", "reasoning": "high", "text": "do the thing"},
        )

    def test_load_workflow_file_reads_agent_defaults(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "defaults_workflow.py"
            path.write_text(
                "WORKFLOW_NAME = 'defaults_workflow'\n"
                "AGENT_DEFAULTS = {'model': 'claude-opus-4-8', 'reasoning': 'high'}\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            registered = host.load_workflow_file(path)

        assert registered is not None
        self.assertEqual(
            registered.agent_defaults,
            {"model": "claude-opus-4-8", "reasoning": "high"},
        )

    def test_load_workflow_file_reads_workflow_principal(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "principal_workflow.py"
            path.write_text(
                "WORKFLOW_NAME = 'principal_workflow'\n"
                "WORKFLOW_PRINCIPAL = True\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            registered = host.load_workflow_file(path)

        assert registered is not None
        self.assertEqual(host.normalize_principal(registered), True)

    def test_load_workflow_file_reads_workflow_principal_foreign_id(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "principal_workflow.py"
            path.write_text(
                "WORKFLOW_NAME = 'principal_workflow'\n"
                "WORKFLOW_PRINCIPAL = ' finance-automation '\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            registered = host.load_workflow_file(path)

        assert registered is not None
        self.assertEqual(host.normalize_principal(registered), "finance-automation")

    def test_workflow_name_from_source_reads_string_constant(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "named.py"
            path.write_text(
                "WORKFLOW_NAME: str = 'annotated_workflow'\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            self.assertEqual(host.workflow_name_from_source(path), "annotated_workflow")

            path.write_text(
                "WORKFLOW_NAME = 'x' + 'y'\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            self.assertIsNone(host.workflow_name_from_source(path))

    def test_discover_skips_disallowed_workflows_without_importing(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            marker = tmp_path / "imported.marker"
            (tmp_path / "blocked.py").write_text(
                f"open({str(marker)!r}, 'w').write('imported')\n"
                "WORKFLOW_NAME = 'blocked_workflow'\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            (tmp_path / "allowed.py").write_text(
                "WORKFLOW_NAME = 'allowed_workflow'\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            with patch.dict(
                os.environ,
                {
                    "WORKFLOW_DIRS": tmp,
                    "WORKFLOW_ENABLE_MODE": "allowlist",
                    "WORKFLOW_ALLOWED_NAMES": "allowed_workflow",
                },
                clear=False,
            ):
                discovered = host.discover_workflows()

        self.assertEqual(set(discovered), {"allowed_workflow"})
        self.assertFalse(marker.exists())

    def test_discover_loads_allowed_workflows_in_allowlist_mode(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "allowed.py").write_text(
                "WORKFLOW_NAME = 'allowed_workflow'\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            with patch.dict(
                os.environ,
                {
                    "WORKFLOW_DIRS": tmp,
                    "WORKFLOW_ENABLE_MODE": "allowlist",
                    "WORKFLOW_ALLOWED_NAMES": "allowed_workflow,other",
                },
                clear=False,
            ):
                discovered = host.discover_workflows()

        self.assertEqual(set(discovered), {"allowed_workflow"})

    def test_discover_skips_non_constant_workflow_name_without_importing(self) -> None:
        host = load_workflow_host()
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            marker = tmp_path / "imported.marker"
            (tmp_path / "dynamic.py").write_text(
                f"open({str(marker)!r}, 'w').write('imported')\n"
                "WORKFLOW_NAME = 'dynamic' + '_workflow'\n"
                "def handler(inp, ctx):\n"
                "    return None\n"
            )
            with patch.dict(
                os.environ,
                {
                    "WORKFLOW_DIRS": tmp,
                    "WORKFLOW_ENABLE_MODE": "all",
                },
                clear=False,
            ):
                discovered = host.discover_workflows()

        self.assertEqual(discovered, {})
        self.assertFalse(marker.exists())

    def test_failed_workflow_host_exits_with_stdin_open(self) -> None:
        source = (
            "WORKFLOW_NAME = 'failing_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    raise RuntimeError('boom')\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "failing_workflow",
                    "input": {},
                },
            )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.error")
            self.assertEqual(response["message"], "boom")
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_returns_result_after_context_response(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    result = await ctx.agent_turn('summarize this')\n"
            "    return {'agent_result': result}\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "agent_workflow",
                    "input": {},
                },
            )

            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.agent_turn")
            self.send_host_message(
                proc,
                {
                    "type": "ctx.response",
                    "request_id": request["request_id"],
                    "ok": True,
                    "value": {"text": "daily digest"},
                },
            )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(
                response["result"],
                {"agent_result": {"text": "daily digest"}},
            )
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_round_trips_agent_batch_result(self) -> None:
        source = (
            "WORKFLOW_NAME = 'review_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    return await ctx.run_agents([\n"
            "        {'name': 'correctness', 'text': 'Review correctness'},\n"
            "        {'name': 'security', 'text': 'Review security'},\n"
            "    ], max_concurrency=2)\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "review_workflow",
                    "input": {},
                },
            )

            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.run_agents")
            self.assertEqual(request["max_concurrency"], 2)
            self.assertEqual(
                [agent["name"] for agent in request["agents"]],
                ["correctness", "security"],
            )
            result = {
                "results": [
                    {
                        "index": 0,
                        "name": "correctness",
                        "ok": True,
                        "result": {"result_text": "looks good"},
                    },
                    {
                        "index": 1,
                        "name": "security",
                        "ok": False,
                        "error": "agent unavailable",
                    },
                ],
                "succeeded": 1,
                "failed": 1,
            }
            self.send_host_message(
                proc,
                {
                    "type": "ctx.response",
                    "request_id": request["request_id"],
                    "ok": True,
                    "value": result,
                },
            )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(response["result"], result)
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_returns_error_after_failed_context_response(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    return await ctx.agent_turn('summarize this')\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "agent_workflow",
                    "input": {},
                },
            )

            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.agent_turn")
            self.send_host_message(
                proc,
                {
                    "type": "ctx.response",
                    "request_id": request["request_id"],
                    "ok": False,
                    "error": "agent unavailable",
                },
            )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.error")
            self.assertEqual(response["message"], "agent unavailable")
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_finishes_active_workflow_after_stdin_eof(self) -> None:
        source = (
            "import asyncio\n"
            "WORKFLOW_NAME = 'slow_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    await asyncio.sleep(0.05)\n"
            "    return {'done': True}\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "slow_workflow",
                    "input": {},
                },
            )
            assert proc.stdin is not None
            proc.stdin.close()

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(response["result"], {"done": True})
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_malformed_input_cancels_active_workflow_cleanly(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    return await ctx.agent_turn('summarize this')\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "agent_workflow",
                    "input": {},
                },
            )
            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.agent_turn")
            assert proc.stdin is not None
            proc.stdin.write("this is not JSON\n")
            proc.stdin.flush()

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "host.error")
            self.assertIn("invalid workflow host input", response["message"])
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 1)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_concurrent_start_does_not_interrupt_active_workflow(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    return await ctx.agent_turn('summarize this')\n"
        )
        start = {
            "type": "workflow.start",
            "run_id": "run-123",
            "task_id": "task-456",
            "workflow_name": "agent_workflow",
            "input": {},
        }
        with self.workflow_host(source) as proc:
            self.send_host_message(proc, start)
            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.agent_turn")

            self.send_host_message(proc, start)
            rejection = self.read_host_message(proc)
            self.assertEqual(rejection["type"], "workflow.error")
            self.assertEqual(
                rejection["message"],
                "workflow host already has an active workflow",
            )

            self.send_host_message(
                proc,
                {
                    "type": "ctx.response",
                    "request_id": request["request_id"],
                    "ok": True,
                    "value": {"text": "done"},
                },
            )
            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(response["result"], {"text": "done"})
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_handles_multiple_context_responses(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    first = await ctx.agent_turn('first')\n"
            "    second = await ctx.agent_turn('second')\n"
            "    return {'first': first, 'second': second}\n"
        )
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "agent_workflow",
                    "input": {},
                },
            )
            for expected_prompt in ("first", "second"):
                request = self.read_host_message(proc)
                self.assertEqual(request["type"], "ctx.agent_turn")
                self.assertEqual(request["args"]["text"], expected_prompt)
                self.send_host_message(
                    proc,
                    {
                        "type": "ctx.response",
                        "request_id": request["request_id"],
                        "ok": True,
                        "value": {"text": f"{expected_prompt} result"},
                    },
                )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(
                response["result"],
                {
                    "first": {"text": "first result"},
                    "second": {"text": "second result"},
                },
            )
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_completion_wins_when_more_input_is_buffered(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    return await ctx.agent_turn('summarize this')\n"
        )
        start = {
            "type": "workflow.start",
            "run_id": "run-123",
            "task_id": "task-456",
            "workflow_name": "agent_workflow",
            "input": {},
        }
        with self.workflow_host(source) as proc:
            self.send_host_message(proc, start)
            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.agent_turn")

            assert proc.stdin is not None
            context_response = {
                "type": "ctx.response",
                "request_id": request["request_id"],
                "ok": True,
                "value": {"text": "done"},
            }
            proc.stdin.write(json.dumps(context_response) + "\n")
            proc.stdin.write(json.dumps(start) + "\n")
            proc.stdin.flush()

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(response["result"], {"text": "done"})
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_accepts_large_start_input(self) -> None:
        source = (
            "WORKFLOW_NAME = 'large_input_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    return {'size': len(inp['payload'])}\n"
        )
        payload = "x" * (128 * 1024)
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "large_input_workflow",
                    "input": {"payload": payload},
                },
            )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(response["result"], {"size": len(payload)})
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")

    def test_workflow_host_accepts_large_context_response(self) -> None:
        source = (
            "WORKFLOW_NAME = 'agent_workflow'\n"
            "async def handler(inp, ctx):\n"
            "    result = await ctx.agent_turn('return a large result')\n"
            "    return {'size': len(result['text'])}\n"
        )
        result_text = "x" * (128 * 1024)
        with self.workflow_host(source) as proc:
            self.send_host_message(
                proc,
                {
                    "type": "workflow.start",
                    "run_id": "run-123",
                    "task_id": "task-456",
                    "workflow_name": "agent_workflow",
                    "input": {},
                },
            )
            request = self.read_host_message(proc)
            self.assertEqual(request["type"], "ctx.agent_turn")
            self.send_host_message(
                proc,
                {
                    "type": "ctx.response",
                    "request_id": request["request_id"],
                    "ok": True,
                    "value": {"text": result_text},
                },
            )

            response = self.read_host_message(proc)
            self.assertEqual(response["type"], "workflow.result")
            self.assertEqual(response["result"], {"size": len(result_text)})
            proc.wait(timeout=2)
            self.assertEqual(proc.returncode, 0)
            assert proc.stderr is not None
            self.assertEqual(proc.stderr.read(), "")


if __name__ == "__main__":
    unittest.main()
