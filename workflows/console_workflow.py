"""Run one Console scheduled task and deliver the response to Slack."""

from __future__ import annotations

import json
from typing import Any

WORKFLOW_NAME = "console_workflow"
SLACK_MESSAGE_MAX_LENGTH = 50_000


def _required_string(params: Any, key: str) -> str:
    if not isinstance(params, dict):
        raise TypeError("console_workflow input must be an object")
    value = params.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"console_workflow requires {key}")
    return value.strip()


async def _deliver_to_slack(ctx: Any, channel: str, text: str) -> Any:
    truncated = text[:SLACK_MESSAGE_MAX_LENGTH]
    return await ctx.step(
        "post_result",
        lambda: ctx.post_to_slack(channel, truncated),
    )


def _response_text(result: Any) -> str:
    if not isinstance(result, dict):
        return ""

    for line in reversed(result.get("output_lines") or []):
        if not isinstance(line, str):
            continue
        try:
            event = json.loads(line)
        except (TypeError, json.JSONDecodeError):
            continue
        if not isinstance(event, dict):
            continue

        method = event.get("method")
        event_type = event.get("type")
        if method != "item/completed" and event_type != "item.completed":
            continue
        params = event.get("params") if isinstance(event.get("params"), dict) else {}
        item = event.get("item") or params.get("item")
        if not isinstance(item, dict) or item.get("type") not in {"agentMessage", "agent_message"}:
            continue
        if item.get("phase") not in {None, "final_answer", "answer"}:
            continue
        text = item.get("text")
        if isinstance(text, str) and text.strip():
            return text.strip()

    return str(result.get("result_text") or "").strip()


async def handler(params: Any, ctx: Any) -> dict[str, Any]:
    prompt = _required_string(params, "prompt")
    principal = _required_string(params, "principal")
    channel = _required_string(params, "channel")
    scheduled_task_id = _required_string(params, "scheduled_task_id")

    result = await ctx.agent_turn(
        prompt,
        principal=principal,
        metadata={
            "scheduled_task_id": scheduled_task_id,
            "scheduled_task_name": str(params.get("scheduled_task_name") or ""),
        },
    )
    response_text = _response_text(result)
    if not response_text:
        response_text = "The task completed without a text response."
    delivery = await _deliver_to_slack(ctx, channel, response_text)

    return {
        "agent_result": result,
        "delivery": delivery,
        "scheduled_task_id": scheduled_task_id,
    }
