"""Run one Console-authored agent prompt and deliver the response to Slack."""

from __future__ import annotations

from typing import Any

WORKFLOW_NAME = "console_workflow"
SLACK_MESSAGE_CHUNK_SIZE = 3500


def _required_string(params: Any, key: str) -> str:
    if not isinstance(params, dict):
        raise TypeError("console_workflow input must be an object")
    value = params.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"console_workflow requires {key}")
    return value.strip()


def _slack_chunks(text: str) -> list[str]:
    return [
        text[offset : offset + SLACK_MESSAGE_CHUNK_SIZE]
        for offset in range(0, len(text), SLACK_MESSAGE_CHUNK_SIZE)
    ]


async def _deliver_to_slack(ctx: Any, channel: str, text: str) -> list[Any]:
    deliveries = []
    thread_ts = None
    for index, chunk in enumerate(_slack_chunks(text), start=1):
        kwargs = {"thread_ts": thread_ts} if thread_ts else {}
        delivery = await ctx.step(
            f"post_result_{index}",
            lambda chunk=chunk, kwargs=kwargs: ctx.post_to_slack(
                channel,
                chunk,
                **kwargs,
            ),
        )
        deliveries.append(delivery)
        if thread_ts is None and isinstance(delivery, dict):
            thread_ts = delivery.get("ts") or None
    return deliveries


async def handler(params: Any, ctx: Any) -> dict[str, Any]:
    prompt = _required_string(params, "prompt")
    principal = _required_string(params, "principal")
    channel = _required_string(params, "channel")
    authored_workflow_id = _required_string(params, "authored_workflow_id")

    result = await ctx.agent_turn(
        prompt,
        principal=principal,
        metadata={
            "source": "console_workflow",
            "authored_workflow_id": authored_workflow_id,
            "authored_workflow_name": str(params.get("authored_workflow_name") or ""),
        },
    )
    response_text = str(result.get("result_text") or "").strip()
    if not response_text:
        response_text = "The workflow completed without a text response."
    deliveries = await _deliver_to_slack(ctx, channel, response_text)

    return {
        "agent_result": result,
        "delivery": deliveries[0],
        "deliveries": deliveries,
        "authored_workflow_id": authored_workflow_id,
    }
