---
title: Creating Workflows
description: Add durable Centaur workflows with checkpointed steps, sleeps, events, child workflows, and agent turns.
---

# Creating Workflows

Workflows are Python handlers that run through Centaur's durable workflow
engine. They are useful when the task is longer than one agent turn: polling,
branching, retries, waiting for external events, or coordinating multiple agent
runs.

Put organization workflows in an overlay repo under `workflows/`. See
[Using an overlay](/extend/overlay) for packaging, mount paths, and chart
configuration.

Workflows are loaded from `WORKFLOW_DIRS`. In an overlay deployment, workflow
files must exist under `/app/overlay/org/workflows` in the API container. Files
in those directories are loaded the same way as built-in workflows.

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
| `ctx.start_workflow(...)` | Start a child workflow and continue immediately. |
| `ctx.wait_for_workflow(...)` | Wait for a child workflow to finish. |
| `ctx.run_workflow(...)` | Start and wait in one call. |
| `ctx.start_agent(...)` | Start an agent turn. |
| `ctx.run_agent(...)` | Start an agent turn and wait for the result. |

The handler may re-execute after a restart. Put external side effects behind
`ctx.step(...)` so completed work is not repeated.

## Run a workflow

Create a run through the API:

```bash
curl -s "$CENTAUR_API_URL/workflows/runs" \
  -H "Content-Type: application/json" \
  -H "X-Api-Key: $CENTAUR_API_KEY" \
  -d '{
    "workflow_name": "nightly_report",
    "input": {"channel": "ops", "topic": "open incidents"},
    "eager_start": true
  }' | jq
```

Inspect it:

```bash
curl -s "$CENTAUR_API_URL/workflows/runs/$RUN_ID" \
  -H "X-Api-Key: $CENTAUR_API_KEY" | jq
```

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
`WORKFLOW_DIRS`, the overlay image contents, and whether the file exports
`WORKFLOW_NAME`. For webhooks, also check for
`workflow_webhook_registered` in the API logs and send a signed request to the
public `/api/webhooks/{slug}` URL.
