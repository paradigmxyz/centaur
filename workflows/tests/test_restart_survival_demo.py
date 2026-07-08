"""Replay/resume semantics for the restart_survival_demo workflow.

Runs the real workflow handler through the real workflow-host
``WorkflowContext`` against an in-memory stand-in for the Absurd checkpoint
store. Every durable sleep suspends (the host process dies and the handler
replays from the top in a fresh process), mirroring what api-rs does in
production — including across an api-rs pod restart, where the checkpoint
store survives in Postgres while every process is replaced.
"""

from __future__ import annotations

import asyncio
import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = REPO_ROOT / "workflows" / "restart_survival_demo.py"

sys.path.insert(0, str(REPO_ROOT / "services" / "workflow-python"))

import workflow_host  # noqa: E402


class SimulatedSuspend(BaseException):
    """Models the workflow-host process dying on a durable sleep suspend."""


class DurableCheckpointRpc:
    """In-memory Absurd stand-in: checkpoints survive, processes do not."""

    def __init__(self) -> None:
        self.checkpoints: dict[str, object] = {}
        self.step_executions: dict[str, int] = {}
        self.notifications: list[dict[str, object]] = []

    def notify(self, payload: dict[str, object]) -> None:
        self.notifications.append(payload)

    async def request(self, payload: dict[str, object]) -> object:
        message_type = payload["type"]
        if message_type == "ctx.step.get":
            step = payload["step"]
            if step in self.checkpoints:
                return {
                    "done": True,
                    "checkpoint_name": step,
                    "value": self.checkpoints[step],
                }
            return {"done": False, "checkpoint_name": step}
        if message_type == "ctx.step.put":
            name = payload["checkpoint_name"]
            self.checkpoints[name] = payload["value"]
            self.step_executions[name] = self.step_executions.get(name, 0) + 1
            return payload["value"]
        if message_type == "ctx.sleep":
            step = payload["step"]
            if step in self.checkpoints:
                return {"slept": True}
            self.checkpoints[step] = {"wake_at": "recorded"}
            raise SimulatedSuspend(step)
        raise AssertionError(f"unexpected request {payload}")


class RestartSurvivalDemoTests(unittest.TestCase):
    def _load_workflow(self) -> workflow_host.RegisteredWorkflow:
        registered = workflow_host.load_workflow_file(WORKFLOW_PATH)
        assert registered is not None
        return registered

    def test_discovered_with_expected_contract(self) -> None:
        registered = self._load_workflow()
        self.assertEqual(registered.workflow_name, "restart_survival_demo")
        self.assertIsNotNone(registered.input_cls)
        self.assertIsNone(registered.webhooks)
        self.assertIsNone(registered.schedule)

    def test_steps_resume_from_checkpoints_across_host_processes(self) -> None:
        rpc = DurableCheckpointRpc()
        raw_input = {"steps": 3, "sleep_seconds": 5}
        passes = 0
        result = None

        while result is None:
            passes += 1
            self.assertLessEqual(passes, 10, "workflow never completed")
            # Fresh module load per pass: a new host process with a new
            # _HOST_PROCESS_ID, exactly like a post-suspend or post-restart
            # replay in production.
            registered = self._load_workflow()
            ctx = workflow_host.WorkflowContext(
                rpc,
                run_id=f"run-pass-{passes}",
                task_id="task-1",
                workflow_name=registered.workflow_name,
            )
            inp = workflow_host.coerce_input(raw_input, registered.input_cls)

            async def run_pass(handler=registered.handler, inp=inp, ctx=ctx):
                try:
                    return await handler(inp, ctx)
                except SimulatedSuspend:
                    return None

            result = asyncio.run(run_pass())

        # 3 steps + 2 sleeps, each sleep suspends once: 3 passes total.
        self.assertEqual(passes, 3)
        markers = result["steps"]
        self.assertEqual([marker["step"] for marker in markers], [1, 2, 3])

        # Every step executed exactly once despite the handler replaying
        # from the top on each pass.
        self.assertEqual(
            rpc.step_executions,
            {"step_1_marker": 1, "step_2_marker": 1, "step_3_marker": 1},
        )

        # Markers checkpointed in earlier passes came back verbatim: each
        # pass ran in a distinct host process, and each marker retains the
        # process that actually executed it.
        host_processes = [marker["host_process_id"] for marker in markers]
        self.assertEqual(len(set(host_processes)), 3)
        run_ids = [marker["absurd_run_id"] for marker in markers]
        self.assertEqual(run_ids, ["run-pass-1", "run-pass-2", "run-pass-3"])
        self.assertTrue(result["resumed_across_host_processes"])
        self.assertNotEqual(
            markers[0]["host_process_id"], result["completed_by_host_process_id"]
        )

    def test_completed_markers_survive_simulated_pod_restart(self) -> None:
        rpc = DurableCheckpointRpc()
        raw_input = {"steps": 3, "sleep_seconds": 5}

        registered = self._load_workflow()
        ctx = workflow_host.WorkflowContext(
            rpc,
            run_id="run-before-restart",
            task_id="task-1",
            workflow_name=registered.workflow_name,
        )
        inp = workflow_host.coerce_input(raw_input, registered.input_cls)

        async def first_pass():
            try:
                return await registered.handler(inp, ctx)
            except SimulatedSuspend:
                return None

        self.assertIsNone(asyncio.run(first_pass()))
        marker_before = rpc.checkpoints["step_1_marker"]

        # "Pod restart": every process is gone; only the checkpoint rows
        # (rpc.checkpoints) survive, exactly like absurd.c_centaur_workflows
        # surviving an api-rs pod kill.
        result = None
        while result is None:
            registered = self._load_workflow()
            ctx = workflow_host.WorkflowContext(
                rpc,
                run_id="run-after-restart",
                task_id="task-1",
                workflow_name=registered.workflow_name,
            )
            inp = workflow_host.coerce_input(raw_input, registered.input_cls)

            async def next_pass(handler=registered.handler, inp=inp, ctx=ctx):
                try:
                    return await handler(inp, ctx)
                except SimulatedSuspend:
                    return None

            result = asyncio.run(next_pass())

        self.assertEqual(result["steps"][0], marker_before)
        self.assertEqual(rpc.step_executions["step_1_marker"], 1)
        self.assertEqual(result["steps"][1]["absurd_run_id"], "run-after-restart")


if __name__ == "__main__":
    unittest.main()
