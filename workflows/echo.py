"""Echo workflow — exercises the per-run sandbox path without external deps.

Useful as a smoke test for the workflow pipeline: trigger a run, watch the
API claim it, watch the executor pod spawn, confirm the handler ran and
status flipped to ``completed``.
"""

from __future__ import annotations

import datetime as dt
import os
import socket
from typing import Any

from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "echo"


async def handler(params: Any, ctx: WorkflowContext) -> dict[str, Any]:
    return {
        "echoed": params,
        "executed_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "hostname": socket.gethostname(),
        "centaur_workflow_run_id": os.environ.get("CENTAUR_WORKFLOW_RUN_ID"),
    }
