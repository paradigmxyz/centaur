---
title: Creating Workflows
description: Add durable Centaur workflows with checkpointed steps, sleeps, events, child workflows, and agent turns.
---

# Creating Workflows

Workflows are Python handlers that run through Centaur's durable workflow
engine. They are useful when the task is longer than one agent turn: polling,
branching, retries, waiting for external events, or coordinating multiple agent
runs.

Use a workflow when the system needs durable progress rather than a single
request-response turn. Common examples include scheduled reports, ETL syncs,
incident monitors, approval gates, webhook-driven triage, long-running research
jobs, and multi-agent handoffs that need to survive deploys or sandbox restarts.

Put organization workflows in an overlay repo under `workflows/`. See
[Using an overlay](/extend/overlay) for packaging, mount paths, and chart
configuration.

Migrating existing workflows to the api-rs Absurd runtime? See
[Workflows v2 Migration](/extend/workflows-v2).

Workflows are loaded from `WORKFLOW_DIRS`. In an overlay deployment, workflow
files must exist under the source's `workflowsSubdir` — by default
`workflows/` — in its repo-cache checkout, for example
`/var/lib/centaur/repos/your-org/centaur-overlay/workflows` in the API
container. Workflow-host sandboxes receive the same ordered list translated to
`/home/agent/github/...`. Files in those directories are loaded the same way as
built-in workflows; sources without the directory are skipped.

## Define a workflow

Each workflow file exports `WORKFLOW_NAME` and an async `handler(params, ctx)`.
An optional `Input` dataclass gives structured inputs.

```python
from dataclasses import dataclass
from datetime import timedelta
from typing import Any

from api.workflow_engine import WorkflowContext


WORKFLOW_NAME = "nightly_report"


@dataclass
class Input:
    channel: str
    topic: str


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    data = await ctx.step("collect", lambda: {"topic": inp.topic})
    await ctx.sleep("settle", timedelta(seconds=30))
    result = await ctx.run_agent(
        "summarize",
        text=f"Write a short report about {data['topic']}",
    )
    return {"channel": inp.channel, "report": result}
```

## Durable primitives

| Primitive | Use it for |
|-----------|------------|
| `ctx.step(name, fn)` | Run a side effect once and cache its result. |
| `ctx.sleep(name, duration)` | Suspend and resume later. |
| `ctx.sleep_until(name, when)` | Resume at a specific time. |
| `ctx.wait_for_event(name, event_type, correlation_id)` | Wait for an external event. |
| `ctx.start_workflow(workflow_name, input, idempotency_key=...)` | Queue a child workflow and continue immediately; returns its durable task/run identifiers. |
| `ctx.start_agent(...)` | Start an agent turn. |
| `ctx.run_agent(...)` | Start an agent turn and wait for the result. |

The handler may re-execute after a restart. Put external side effects behind
`ctx.step(...)` so completed work is not repeated.

These primitives compose into larger automations:

- **Scheduled operations**: run a daily digest, weekly cleanup, periodic sync,
  or business-hours monitor without a human prompt.
- **Polling loops**: sleep between checks for CI, blockchain confirmations,
  billing state, deploy health, or vendor exports.
- **Event-driven flows**: wait for a webhook, approval, upload, or callback and
  continue from the last checkpoint.
- **Fan-out/fan-in orchestration**: start child workflows for independent work
  and wait for all of them before producing a final result.
- **Agent orchestration**: use agents for judgment-heavy steps while the
  workflow owns timing, retries, state, and final delivery.

## Run a workflow

Create a run through the API:

```bash
curl -s "$CENTAUR_API_URL/api/workflows/runs" \
  -H "Content-Type: application/json" \
  -d '{
    "workflow_name": "nightly_report",
    "input": {"channel": "ops", "topic": "open incidents"},
    "eager_start": true
  }' | jq
```

Inspect it:

```bash
curl -s "$CENTAUR_API_URL/api/workflows/runs/$RUN_ID" | jq
```

## Schedule a workflow

Workflows can run from schedule metadata declared beside the handler. Use this
when a workflow should be started by the platform on a clock instead of by an
API call or webhook.

```python
WORKFLOW_NAME = "daily_market_digest"

SCHEDULE = {
    "type": "cron",
    "cron": "0 9 * * MON-FRI",
    "timezone": "America/New_York",
    "input": {
        "channel": "markets",
        "topic": "overnight market structure and portfolio-relevant news",
    },
}
```

Cron schedules use five fields:

```text
minute hour day-of-month month day-of-week
```

:::warning[Day-of-week numbering is Quartz-style, not Unix crontab]
The schedule engine parses cron expressions with the Rust
[`cron` crate](https://github.com/zslayton/cron), which numbers days of week
1–7 with **1 = Sunday** (`0` is rejected). A Unix-style `1-5` therefore fires
Sunday–Thursday, not Monday–Friday. Always write day-of-week as names
(`MON`, `MON-FRI`, `SAT,SUN`) — they mean the same thing in every dialect.
:::

Examples:

| Cron | Meaning |
|------|---------|
| `0 9 * * MON-FRI` | 9:00 AM every weekday. |
| `0 9 * * 1-5` | 9:00 AM Sunday–Thursday (Quartz numbering — probably not what you meant). |
| `*/15 * * * *` | Every 15 minutes. |
| `30 6 * * *` | 6:30 AM every day. |
| `0 0 1 * *` | Midnight on the first day of every month. |

Always set `timezone` for human-facing schedules. Without an explicit timezone,
cron expressions are easy to misread across daylight saving changes and
deployments in different regions.

Use `input` to keep the handler deterministic for scheduled runs: channel names,
query scopes, tenant IDs, lookback windows, and delivery settings should be
declared in the schedule instead of inferred from wall-clock state when
possible.

For workflows that may run longer than their schedule interval, make each tick
idempotent. Put writes and external API calls in named `ctx.step(...)` blocks,
derive stable keys from the scheduled window, and have the handler detect
already-processed periods before starting expensive work.

Interval schedules are useful when exact wall-clock alignment does not matter:

```python
SCHEDULE = {
    "type": "interval",
    "seconds": 300,
    "input": {"target": "production"},
}
```

Use cron for calendar semantics such as "weekday at 9 AM"; use intervals for
continuous monitors such as "check every five minutes".

## Expose a workflow as a webhook

Workflows are private unless the workflow file explicitly exports `WEBHOOKS`.
Each webhook is mounted at `POST /api/webhooks/{slug}` and creates a durable
workflow run with a normalized webhook envelope. Use this for provider-driven
entrypoints such as GitHub issue triage, billing events, or deploy callbacks.

```python
from typing import Any

from api.webhooks import HeaderTriggerKey, HmacAuth, WebhookSpec
from api.workflow_engine import WorkflowContext


WORKFLOW_NAME = "github_issue_triage"

WEBHOOKS = [
    WebhookSpec(
        slug="github-issue-triage",
        provider="github",
        auth=HmacAuth.github(secret_ref="GITHUB_WEBHOOK_SECRET"),
        trigger_key=HeaderTriggerKey("X-GitHub-Delivery"),
        allowed_methods=["POST"],
        allowed_content_types=[
            "application/json",
            "application/x-www-form-urlencoded",
        ],
    )
]


async def handler(inp: dict[str, Any], ctx: WorkflowContext) -> dict[str, Any]:
    webhook = inp["webhook"]
    headers = webhook["headers"]
    payload = webhook["body"]

    if headers.get("x-github-event") != "issues":
        return {"skipped": True, "reason": "unsupported_event"}

    issue = payload["issue"]
    repo = payload["repository"]["full_name"]
    result = await ctx.agent_turn(
        f"Triage GitHub issue {repo}#{issue['number']}: {issue['title']}",
        thread_key=f"github:{repo}:{issue['number']}",
    )
    return {"triaged": True, "agent_result": result}
```

Configure the provider to call:

```text
https://<your-centaur-host>/api/webhooks/github-issue-triage
```

For GitHub, set the webhook secret to the same value as
`GITHUB_WEBHOOK_SECRET` in the API deployment and select `application/json`.
GitHub's default `application/x-www-form-urlencoded` payloads also work when
that content type is listed in `allowed_content_types`.

Webhook requests do not use Centaur API keys. The API verifies the provider
signature before creating workflow state. `HmacAuth.github(...)` verifies
`X-Hub-Signature-256`; a plain `HmacAuth(...)` can be used for other
SHA-256 HMAC providers. During local development or for trusted internal
routes, `auth="none"` is allowed.

The workflow receives input in this shape:

```json
{
  "webhook": {
    "slug": "github-issue-triage",
    "provider": "github",
    "method": "POST",
    "path": "/api/webhooks/github-issue-triage",
    "headers": {
      "x-github-event": "issues",
      "x-github-delivery": "..."
    },
    "query": {},
    "body": {},
    "raw_body_sha256": "...",
    "source_ip": "203.0.113.10"
  }
}
```

Sensitive headers such as signatures, cookies, authorization, and API keys are
removed before the workflow input is persisted. `trigger_key` controls
idempotency; prefer a provider delivery header like `X-GitHub-Delivery`. If no
trigger key is configured, Centaur uses the raw body SHA-256 hash.

The webhook endpoint returns `202` when it creates a new run and `200` when the
same trigger key maps to an existing run.

## Verify

After deploying an overlay, check API logs for workflow load events and create a
small run with `eager_start: true`. If the workflow is missing, inspect
`WORKFLOW_DIRS`, the configured repo/ref in repo-cache, and whether the file exports
`WORKFLOW_NAME`. For webhooks, also check for
`workflow_webhook_registered` in the API logs and send a signed request to the
public `/api/webhooks/{slug}` URL.
