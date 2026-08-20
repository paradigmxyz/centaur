"""Run one Console-authored agent prompt and deliver the response to Slack."""

from __future__ import annotations

from typing import Any

WORKFLOW_NAME = "console_workflow"


def _required_string(params: Any, key: str) -> str:
    if not isinstance(params, dict):
        raise ValueError("console_workflow input must be an object")
    value = params.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"console_workflow requires {key}")
    return value.strip()


async def handler(params: Any, ctx: Any) -> dict[str, Any]:
    prompt = _required_string(params, "prompt")
    principal = _required_string(params, "principal")
    channel = _required_string(params, "channel")
    authored_workflow_id = _required_string(params, "authored_workflow_id")

    result = await ctx.agent_turn(
        prompt,
        principal=principal,
        thread_key=f"console-workflow:{authored_workflow_id}:{ctx.run_id}",
        metadata={
            "source": "console_workflow",
            "authored_workflow_id": authored_workflow_id,
            "authored_workflow_name": str(params.get("authored_workflow_name") or ""),
        },
    )
    response_text = str(result.get("result_text") or "").strip()
    if not response_text:
        response_text = "The workflow completed without a text response."
    delivery = await ctx.post_to_slack(channel, response_text)

    return {
        "agent_result": result,
        "delivery": delivery,
        "authored_workflow_id": authored_workflow_id,
    }
