#!/usr/bin/env bash
set -euo pipefail

namespace="${CENTAUR_NAMESPACE:-centaur}"
release="${CENTAUR_RELEASE:-centaur}"
link="${1:-}"

if [[ "$link" =~ /archives/([^/]+)/p([0-9]+) ]]; then
  channel="${BASH_REMATCH[1]}"
  raw_ts="${BASH_REMATCH[2]}"
else
  echo "expected Slack archive URL like https://app.slack.com/archives/C9Q8R7S6T5/p1234567890123456" >&2
  exit 2
fi
ts_exact="${raw_ts:0:10}.${raw_ts:10}"

kubectl -n "$namespace" exec -i "deploy/${release}-centaur-api" -- \
  env SLACK_CHANNEL="$channel" SLACK_TS="$ts_exact" uv run python - <<'PY'
import asyncio
import json
import os

import asyncpg

channel = os.environ["SLACK_CHANNEL"]
ts = os.environ["SLACK_TS"]
thread_suffix = f":{channel}:{ts}"


def part_text(parts: object) -> str:
    if isinstance(parts, str):
        parts = json.loads(parts)
    if not isinstance(parts, list):
        return ""
    return "\n".join(
        part.get("text", "")
        for part in parts
        if isinstance(part, dict) and part.get("type") == "text"
    ).strip()


def short_output(output: str | None, limit: int = 900) -> str:
    if not output:
        return ""
    output = output.strip()
    return output if len(output) <= limit else output[:limit].rstrip() + "\n... [truncated]"


async def main() -> None:
    conn = await asyncpg.connect(os.environ["DATABASE_URL"])
    thread_key = await conn.fetchval(
        """
        SELECT thread_key
        FROM chat_messages
        WHERE thread_key LIKE $1
        ORDER BY created_at DESC
        LIMIT 1
        """,
        f"%{thread_suffix}",
    )
    if not thread_key:
        print(f"No transcript found for {channel} {ts}")
        await conn.close()
        return

    print("# Slack Thread Report\n")
    print(f"thread_key: {thread_key}\n")

    messages = await conn.fetch(
        """
        SELECT role, user_id, parts, created_at
        FROM chat_messages
        WHERE thread_key = $1
        ORDER BY created_at
        """,
        thread_key,
    )
    print("## Transcript")
    for row in messages:
        text = part_text(row["parts"])
        if text:
            who = row["role"] if not row["user_id"] else f"{row['role']} ({row['user_id']})"
            print(f"\n### {who} @ {row['created_at'].isoformat()}\n{text}")

    execution = await conn.fetchrow(
        """
        SELECT execution_id, status, terminal_reason, result_text, error_text
        FROM agent_execution_requests
        WHERE thread_key = $1
        ORDER BY created_at DESC
        LIMIT 1
        """,
        thread_key,
    )
    if not execution:
        await conn.close()
        return

    print("\n## Execution")
    print(f"execution_id: {execution['execution_id']}")
    print(f"status: {execution['status']}")
    print(f"terminal_reason: {execution['terminal_reason']}")
    if execution["error_text"]:
        print(f"error_text: {execution['error_text']}")

    events = await conn.fetch(
        """
        SELECT event_id, event_json, created_at
        FROM agent_execution_events
        WHERE thread_key = $1 AND execution_id = $2 AND event_kind = 'amp_raw_event'
        ORDER BY event_id
        """,
        thread_key,
        execution["execution_id"],
    )

    commands: list[dict[str, object]] = []
    commentary: list[str] = []
    current_message: list[str] = []
    reasoning_count = 0
    reasoning_with_text = 0
    for row in events:
        event = json.loads(row["event_json"])
        event_type = event.get("type")
        item = event.get("item") or {}
        item_type = item.get("type")
        if item_type == "reasoning" and event_type == "item.completed":
            reasoning_count += 1
            if item.get("content") or item.get("summary"):
                reasoning_with_text += 1
        if item_type == "commandExecution" and event_type == "item.completed":
            commands.append(
                {
                    "at": row["created_at"].isoformat(),
                    "command": item.get("command"),
                    "status": item.get("status"),
                    "exitCode": item.get("exitCode"),
                    "output": item.get("aggregatedOutput"),
                }
            )
        if event_type == "item.agentMessage.delta":
            current_message.append(str(event.get("delta", "")))
        if event_type == "item.completed" and item_type == "agentMessage":
            text = "".join(current_message).strip()
            if text:
                commentary.append(text)
            current_message = []

    print("\n## Command Executions")
    if commands:
        for index, command in enumerate(commands, 1):
            print(f"\n{index}. {command['command']}")
            print(f"   status={command['status']} exitCode={command['exitCode']} at={command['at']}")
            output = short_output(command.get("output") if isinstance(command.get("output"), str) else None)
            if output:
                print("   output:")
                for line in output.splitlines():
                    print(f"     {line}")
    else:
        print("No command executions found.")

    print("\n## Commentary / Visible Progress")
    non_final = (
        commentary[:-1]
        if execution["result_text"] and commentary and commentary[-1] == execution["result_text"]
        else commentary
    )
    if non_final:
        for text in non_final:
            print(f"\n{text}")
    else:
        print("No commentary messages found.")

    print("\n## Thinking")
    if reasoning_count and not reasoning_with_text:
        print(f"{reasoning_count} reasoning event(s), but no stored content/summary text.")
    elif reasoning_count:
        print(f"{reasoning_count} reasoning event(s), {reasoning_with_text} with stored text.")
    else:
        print("No reasoning events found.")

    print("\n## Actual Response Text")
    print(execution["result_text"] or "")

    await conn.close()


asyncio.run(main())
PY
