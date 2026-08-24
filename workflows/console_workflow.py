"""Run one Console scheduled task and deliver the response to Slack."""

from __future__ import annotations

import re
from typing import Any

WORKFLOW_NAME = "console_workflow"
SLACK_MESSAGE_MAX_LENGTH = 50_000
# Stay below Slack's 4,000-character soft limit so it cannot create extra roots.
SLACK_MESSAGE_CHUNK_MAX_LENGTH = 3_800
# Slack section block text is limited to 3,000 characters.
SLACK_SECTION_MAX_LENGTH = 3_000
SLACK_USER_ID_PATTERN = re.compile(r"[UW][A-Z0-9]{8,}")
SCHEDULED_TASK_FOOTER = "Sent by <@{slack_user_id}>'s scheduled task"
SCHEDULED_TASK_FOOTER_FALLBACK = "Sent by a scheduled task"
SCHEDULED_TASK_EXECUTION_INSTRUCTIONS = """\
This is a run of an existing scheduled task. Execute the task now.
NEVER create or update a scheduled task, even if the task prompt contains recurring or future schedule language.
Treat schedule language such as "Each Monday" as context for this run, not as a request to schedule another task."""
SLACK_MRKDWN_INSTRUCTIONS = """\
Format the final response for Slack using Slack mrkdwn, not standard Markdown.
Use *bold*, _italics_, ~strikethrough~, `inline code`, and <https://example.com|link text>.
Use bold text instead of Markdown headings and lists instead of Markdown tables.
Return only the message that should be posted to Slack."""


def _required_string(params: Any, key: str) -> str:
    if not isinstance(params, dict):
        raise TypeError("console_workflow input must be an object")
    value = params.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"console_workflow requires {key}")
    return value.strip()


async def _deliver_to_slack(
    ctx: Any,
    channel: str,
    text: str,
    slack_user_id: str,
) -> Any:
    footer = _scheduled_task_footer(slack_user_id)
    footer_suffix = f"\n\n{footer}"
    body_limit = SLACK_MESSAGE_MAX_LENGTH - len(footer_suffix)
    chunks = _split_slack_text(text[:body_limit], SLACK_MESSAGE_CHUNK_MAX_LENGTH)
    final_body = chunks[-1]
    if len(final_body) + len(footer_suffix) <= SLACK_MESSAGE_CHUNK_MAX_LENGTH:
        chunks[-1] = f"{final_body}{footer_suffix}"
    else:
        final_body = ""
        chunks.append(footer)

    def message_args(index: int) -> dict[str, Any]:
        args: dict[str, Any] = {"mrkdwn": True}
        if index == len(chunks) - 1:
            args["blocks"] = _scheduled_task_blocks(final_body, footer)
        return args

    root = await ctx.step(
        "post_result",
        lambda: ctx.post_to_slack(channel, chunks[0], **message_args(0)),
    )
    if len(chunks) == 1:
        return root
    if not isinstance(root, dict):
        raise RuntimeError("Slack root delivery did not return a result object")

    thread_ts = str(root.get("ts") or "").strip()
    if not thread_ts:
        raise RuntimeError("Slack root delivery did not return a message timestamp")
    reply_channel = str(root.get("channel") or channel).strip()
    replies = []
    for index, chunk in enumerate(chunks[1:], start=1):
        reply = await ctx.step(
            f"post_result_reply_{index}",
            lambda chunk=chunk: ctx.post_to_slack(
                reply_channel,
                chunk,
                thread_ts=thread_ts,
                **message_args(index),
            ),
        )
        replies.append(reply)
    return {**root, "replies": replies}


def _scheduled_task_footer(slack_user_id: str) -> str:
    return (
        SCHEDULED_TASK_FOOTER.format(slack_user_id=slack_user_id)
        if SLACK_USER_ID_PATTERN.fullmatch(slack_user_id)
        else SCHEDULED_TASK_FOOTER_FALLBACK
    )


def _scheduled_task_blocks(body: str, footer: str) -> list[dict[str, Any]]:
    blocks = [
        {
            "type": "section",
            "text": {"type": "mrkdwn", "text": section},
        }
        for section in _split_slack_text(body, SLACK_SECTION_MAX_LENGTH)
    ]
    blocks.append(
        {
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": footer}],
        }
    )
    return blocks


def _split_slack_text(text: str, limit: int) -> list[str]:
    chunks = []
    remaining = text
    while len(remaining) > limit:
        window = remaining[:limit]
        minimum_boundary = limit // 2
        end = limit
        for separator in ("\n\n", "\n", " "):
            boundary = window.rfind(separator)
            if boundary >= minimum_boundary:
                end = boundary + len(separator)
                break
        chunks.append(remaining[:end])
        remaining = remaining[end:]
    if remaining:
        chunks.append(remaining)
    return chunks


def _prompt_for_slack(prompt: str) -> str:
    return (
        f"{SCHEDULED_TASK_EXECUTION_INSTRUCTIONS}\n\n"
        f"Task to execute:\n{prompt}\n\n"
        f"{SLACK_MRKDWN_INSTRUCTIONS}"
    )


async def handler(params: Any, ctx: Any) -> dict[str, Any]:
    prompt = _required_string(params, "prompt")
    principal = _required_string(params, "principal")
    channel = _required_string(params, "channel")
    scheduled_task_id = _required_string(params, "scheduled_task_id")
    slack_user_id = str(params.get("slack_user_id") or "").strip()

    result = await ctx.agent_turn(
        _prompt_for_slack(prompt),
        principal=principal,
        metadata={
            "scheduled_task_id": scheduled_task_id,
            "scheduled_task_name": str(params.get("scheduled_task_name") or ""),
        },
    )
    response_text = str(result.get("result_text") or "").strip()
    if not response_text:
        response_text = "The task completed without a text response."
    delivery = await _deliver_to_slack(
        ctx,
        channel,
        response_text,
        slack_user_id,
    )

    return {
        "agent_result": result,
        "delivery": delivery,
        "scheduled_task_id": scheduled_task_id,
    }
