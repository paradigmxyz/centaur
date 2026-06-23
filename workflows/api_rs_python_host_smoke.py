"""Small Python workflow used to prove the api-rs sandbox workflow host."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "api_rs_python_host_smoke"


@dataclass
class Input:
    message: str = "hello"
    sleep_seconds: float = 0.0


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    async def echo_step() -> dict[str, str]:
        return {"upper": inp.message.upper()}

    echoed = await ctx.step("echo", echo_step)
    if inp.sleep_seconds > 0:
        await asyncio.sleep(inp.sleep_seconds)
    return {
        "message": inp.message,
        "echoed": echoed,
        "workflow_name": ctx.workflow_name,
        "run_id": ctx.run_id,
    }
