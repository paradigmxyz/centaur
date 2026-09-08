"""Example: the workflow decides what Approve and Reject do."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from api.workflow_engine import Button, ButtonClick, WorkflowContext

WORKFLOW_NAME = "button_actions"


@dataclass
class Input:
    team_id: str
    channel: str
    allowed_users: list[str]
    text: str = "Start the example workflow?"
    timeout: int = 86400


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    async def approve(click: ButtonClick) -> Any:
        return await ctx.start_workflow(
            "echo",
            {"approved_by": click.user_id, "action_id": click.id},
            name="approved-echo",
        )

    async def reject(click: ButtonClick) -> Any:
        return {"rejected_by": click.user_id}

    result = await ctx.slack_actions(
        "example-review",
        channel=inp.channel,
        team_id=inp.team_id,
        text=inp.text,
        allowed_users=inp.allowed_users,
        timeout=inp.timeout,
        buttons={
            "approve": Button("Approve", approve, style="primary"),
            "reject": Button("Reject", reject, style="danger"),
        },
    )
    return {"outcome": result.outcome, "value": result.value}
