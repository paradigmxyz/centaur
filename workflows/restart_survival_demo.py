"""Restart-survival demo workflow — proves durable resume across api-rs restarts.

Runs ``steps`` (default 3) checkpointed steps separated by explicit durable
sleeps (default 90s each, ~3 min total wall time). Each step records a marker
(UTC timestamp, workflow-host process id, hostname, pid, Absurd run id) as its
``ctx.step`` return value, so every marker is persisted as an Absurd checkpoint
in Postgres the moment the step completes.

Because ``ctx.sleep`` suspends the Absurd task (the workflow-host process exits
and the handler replays from the top on wake), a marker whose
``host_process_id`` differs from its neighbours proves the run resumed in a new
host process, while a marker that is byte-identical before and after an api-rs
pod restart proves the step was NOT re-executed.

Trigger a run:

    POST /api/workflows/runs
    {"workflow_name": "restart_survival_demo", "input": {"steps": 3, "sleep_seconds": 90}}

Read durable step markers mid-run (task_id from the create response):

    select checkpoint_name, state, updated_at
    from absurd.c_centaur_workflows
    where task_id = '<TASK_ID>'::uuid
    order by updated_at;

Pass criteria for a mid-run api-rs restart: markers checkpointed before the
restart are unchanged afterwards, later steps still execute exactly once, and
the final result lists every step with monotonically increasing timestamps.
"""

from __future__ import annotations

import datetime as dt
import os
import socket
import uuid
from dataclasses import dataclass
from typing import Any

from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "restart_survival_demo"

MAX_STEPS = 10
MAX_SLEEP_SECONDS = 600.0

# Regenerated on every module import, i.e. once per workflow-host process.
# Each suspend/resume cycle (every durable sleep, and any api-rs restart)
# spawns a fresh host process, so completed-step markers keep the process id
# of the process that actually executed them.
_HOST_PROCESS_ID = uuid.uuid4().hex[:12]


@dataclass
class Input:
    steps: int = 3
    sleep_seconds: float = 90.0


def _utc_now_iso() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    steps = max(1, min(int(inp.steps), MAX_STEPS))
    sleep_seconds = max(0.0, min(float(inp.sleep_seconds), MAX_SLEEP_SECONDS))

    ctx.log(
        "restart_survival_demo_pass",
        host_process_id=_HOST_PROCESS_ID,
        hostname=socket.gethostname(),
        steps=steps,
        sleep_seconds=sleep_seconds,
    )

    markers: list[dict[str, Any]] = []
    for index in range(1, steps + 1):

        async def record_marker(step_index: int = index) -> dict[str, Any]:
            return {
                "step": step_index,
                "executed_at": _utc_now_iso(),
                "host_process_id": _HOST_PROCESS_ID,
                "hostname": socket.gethostname(),
                "pid": os.getpid(),
                "absurd_run_id": ctx.run_id,
            }

        marker = await ctx.step(f"step_{index}_marker", record_marker)
        markers.append(marker)
        if index < steps:
            await ctx.sleep(f"pause_after_step_{index}", sleep_seconds)

    host_processes = sorted({marker["host_process_id"] for marker in markers})
    return {
        "workflow_name": WORKFLOW_NAME,
        "steps": markers,
        "completed_at": _utc_now_iso(),
        "completed_by_host_process_id": _HOST_PROCESS_ID,
        "step_host_processes": host_processes,
        "resumed_across_host_processes": len(host_processes) > 1
        or (bool(markers) and markers[-1]["host_process_id"] != _HOST_PROCESS_ID),
    }
