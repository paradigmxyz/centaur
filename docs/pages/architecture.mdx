---
title: Architecture
description: How Centaur runs agents with an API, Postgres, sandbox pods, tools, workflows, and iron-proxy.
---

# Architecture

Centaur accepts Slack and API requests, stores each turn, assigns an isolated
runtime, exposes approved tools, injects credentials through a proxy, and keeps
an event trail clients can replay.

<figure className="architecture-figure">
  <img src="/brand/architecture.svg" alt="Centaur architecture — ingress, durable control plane, isolated execution, tools, workflows, and controlled egress" />
</figure>

## Planes

| Plane | Responsibility | Main components |
|-------|----------------|-----------------|
| Ingress | Accept user and client input. | Slack Events API, Slackbot webhook, external API clients. |
| Control | Persist requests and coordinate runtime state. | FastAPI, Postgres, execution worker. |
| Execution | Run one assigned agent session per thread. | Kubernetes sandbox pods. |
| Capabilities | Give agents approved actions. | Tool plugins, workflow engine, overlays. |
| Secrets and egress | Let agents call third-party APIs without receiving raw keys. | Kubernetes Secret, [iron-proxy](https://docs.iron.sh), per-sandbox proxy token mapping. |

## Durable API lifecycle

Clients do not manage containers or keep long-running processes alive. They call
the API and follow the event stream.

| Step | Endpoint | What it saves |
|------|----------|----------------|
| Start or reuse a sandbox | `POST /agent/spawn` | The thread's current sandbox assignment. |
| Persist input | `POST /agent/message` | Writes the user turn and extracts large multimodal attachments. |
| Run the agent | `POST /agent/execute` | A run row with status and final result. |
| Follow output | `GET /agent/threads/{thread}/events` | Tool calls, model output, status changes, and final text. |
| Clean up | `POST /agent/threads/{thread}/release` | Releases the sandbox and can cancel running work. |

Because each step is stored, a Slack reconnect, browser refresh, API restart,
pod replacement, or worker failover does not erase the run. The event stream is
the client contract; Slack and other clients should reconnect with
`after_event_id` instead of trying to reconstruct state locally.

## Slackbot ingress

Slack talks to Centaur through the Slack Events API. The public request URL is
the Slackbot webhook, usually:

```text
https://api.acme.com/api/webhooks/slack
```

The webhook does not use a Centaur API key. Slack signs every request with
`X-Slack-Signature` and `X-Slack-Request-Timestamp`; the Slackbot validates that
HMAC signature with `SLACK_SIGNING_SECRET` before it routes the event to the API.
After validation, the Slackbot calls Centaur's agent API with
`SLACKBOT_API_KEY`.

During a Slack delivery, the API owns the execution state while Slackbot owns
Slack rendering: opening or updating the thread UI, streaming chunks, rendering
steps, and posting the final answer. The landing page preview shows that Slack
thread surface; the durable API lifecycle above is the system underneath it.

## Execution path

Kubernetes is the active sandbox runtime path. The API creates or claims a
sandbox pod, attaches to it, and runs the requested agent CLI. Do not plan a new
deployment around a local-container backend; the Helm chart, warm pool, overlay
mounting, and network policies all assume Kubernetes sandboxes.

| Harness | Adapter behavior |
|---------|------------------|
| Amp | Materializes image/document blocks to files and passes text plus file references. |
| Claude Code | Passes the Anthropic-shaped content through directly. |
| Codex / pi-mono | Extracts text blocks for CLIs that accept a plain prompt. |

The pod receives the prompt files, CLI command, internal API URL, proxy CA, and
proxy settings. It does not need Kubernetes credentials or long-lived
third-party API keys.

## Tool and workflow layer

Tools are Python plugin directories. Each public client method becomes a REST
method at `/tools/{name}/{method}`. Agents discover tools when they start.

Use tools for search, Slack, GitHub, market data, calendars, internal systems,
and deployment-specific APIs. Tool code should read credentials with
`secret("NAME")` so the same code works locally and in production.

Workflows are Python handlers that save step results. When a worker restarts,
the handler runs again, but `ctx.step(...)` returns cached results for completed
work.

Use workflows for scheduled digests, monitoring loops, approval gates, jobs that
sleep for minutes or days, and parent/child workflow trees.

## Secrets and outbound requests

Agents and tools refer to credentials by name, such as `OPENAI_API_KEY` or
`secret("CRM_API_TOKEN")`. The sandbox container only ever holds those
placeholder names; the real values live on a per-sandbox
[iron-proxy](https://docs.iron.sh) pod, bound to specific upstream hosts
and request locations, and substituted on the wire when an outbound
request matches.

See [Security](/security) for the full threat model and what it does
and does not protect against.

## Failure model

| Failure | Expected recovery |
|---------|-------------------|
| Client disconnects | Reconnect to the event stream with `after_event_id`. |
| API restarts | Reload assignments, executions, and terminal state from Postgres. |
| Sandbox pod dies | The execution becomes terminal, the event trail remains in Postgres, and operators inspect `GET /agent/executions/{execution_id}` plus API/sandbox logs before retrying the turn. |
| Workflow worker restarts | Re-run the handler and skip completed checkpoints. |
| Proxy restarts | Rebuild the key-injection map from the secret-manager cache. |
| Tool changes | Discovery reloads plugin metadata; agents see the updated methods. |
