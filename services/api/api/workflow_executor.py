"""One-shot workflow handler entrypoint — runs inside a per-run sandbox Pod.

Invoked as ``python -m api.workflow_executor --run-id <id>`` by the API's
worker when it claims a pending row from ``workflow_runs``. The sandbox
container fetches the (already-claimed) run row, drives the existing
``_run_handler`` to completion, and exits. All checkpoint / status
bookkeeping happens in the DB exactly as it did when the worker ran the
handler in-process.

The sandbox terminates with exit code 0 on a clean handler return (the
handler itself wrote terminal state to the DB) and a non-zero code if
the executor never got to ``_run_handler`` (e.g. DB unreachable, run row
gone, handler module missing).
"""

from __future__ import annotations

import argparse
import asyncio
import sys

import structlog

from api.config import settings
from api.db import close_pool, create_pool
from api.logging_config import configure_structlog
from api.workflow_engine import (
    _run_handler,
    discover_workflow_handlers,
)

configure_structlog()
log = structlog.get_logger().bind(service="workflow-executor")


async def _run(run_id: str) -> int:
    pool = await create_pool(settings.database_url)
    try:
        discover_workflow_handlers()
        row = await pool.fetchrow(
            "SELECT run_id, workflow_name, input_json, status, "
            "       created_at, worker_id "
            "FROM workflow_runs WHERE run_id = $1",
            run_id,
        )
        if row is None:
            log.error("workflow_run_not_found", run_id=run_id)
            return 2
        run_row = dict(row)
        if str(run_row.get("status") or "") not in ("running", "queued", "waiting", "sleeping"):
            log.error(
                "workflow_run_not_executable",
                run_id=run_id,
                status=run_row.get("status"),
            )
            return 3
        log.info(
            "workflow_executor_starting",
            run_id=run_id,
            workflow_name=run_row.get("workflow_name"),
            status=run_row.get("status"),
        )
        await _run_handler(pool, run_row)
        log.info("workflow_executor_finished", run_id=run_id)
        return 0
    finally:
        await close_pool(pool)


def main() -> int:
    parser = argparse.ArgumentParser(description="Workflow run executor")
    parser.add_argument("--run-id", required=True, help="workflow_runs.run_id")
    args = parser.parse_args()
    return asyncio.run(_run(args.run_id))


if __name__ == "__main__":
    sys.exit(main())
