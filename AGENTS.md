# Centaur — Developer Guide

## Quick Start

### 1. Clone and configure

```bash
git clone <repo-url>
cd centaur
brew install just
```

Centaur runs locally on Kubernetes through the Helm chart. Infra secrets are required as pre-created Kubernetes Secrets. For local development, `just bootstrap-secrets` creates them from your shell environment:

```bash
export OP_SERVICE_ACCOUNT_TOKEN=...
export OP_VAULT=...
export SLACK_BOT_TOKEN=...
export SLACK_SIGNING_SECRET=...
export SLACKBOT_API_KEY=...
```

Application-level LLM/tool secrets such as OpenAI and Anthropic tokens stay in 1Password and are loaded by the secrets service.

### 2. Boot the stack

```bash
just up
```

### Database migrations

api-rs embeds SQLx migrations from
`services/api-rs/crates/centaur-session-sqlx/migrations`. To add schema, create
the next numbered SQL file in that directory and keep it compatible with the
embedded migrator. The api-rs binary applies those migrations on startup when
the chart enables migration running, and the Rust tests use the same migration
set for database-backed coverage.

### 3. Test

From inside the API deployment (localhost bypass — no key needed):

```bash
THREAD_KEY=cli:test-e2e-1
THREAD_PATH=$(jq -rn --arg v "$THREAD_KEY" '$v|@uri')

SESSION=$(kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}" \
  -H "Content-Type: application/json" \
  -d '{"harness_type":"codex","on_harness_conflict":"restart"}')

kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}/messages" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","parts":[{"type":"text","text":"Reply with exactly PONG and nothing else."}]}]}'

EXECUTE=$(kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}/execute" \
  -H "Content-Type: application/json" \
  -d '{"input_lines":["{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG and nothing else.\"}]}}"]}')
EXECUTION_ID=$(printf '%s' "$EXECUTE" | jq -r '.execution_id')

kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -N \
  "http://localhost:8080/api/session/${THREAD_PATH}/events?execution_id=${EXECUTION_ID}&after_event_id=0"
```

Or use the deployment's configured service bearer token path for external
clients (see [API Key Management](#api-key-management)).

## Architecture

See the [architecture diagram in the README](README.md#architecture).

### End-to-End Request Flow

1. User mentions bot in Slack → webhook → slackbotv2 → api-rs
2. api-rs spawns/reuses a Kubernetes sandbox pod (`centaur-agent:latest`) for that thread
3. Executes harness (amp/claude-code/codex) through the sandbox backend
4. Harness calls local tool CLI shims installed by `centaur-tools` (NOT MCP)
5. LLM/API calls route through per-sandbox iron-proxy which injects real credentials
6. Results stream as JSON events → posted to Slack

### Service Interface Contracts

Centaur is a modular service architecture. Each service communicates through well-defined interfaces. As long as you implement these interfaces, you can swap or extend any layer independently.

**Client → API** (durable control-plane protocol):

Clients (slackbotv2, CLI, external integrations) should stay thin. They create
or reuse a session, append durable messages, execute the session, and stream or
replay output from the durable event endpoint. api-rs owns runtime assignment,
execution serialization, cancellation/recovery, and final delivery; Postgres is
the source of truth.

Thread keys are path parameters on the api-rs session routes, so callers must
URL-encode values such as `slack:T123:C456:1773364194.179929`.

**Step 1: Assign or reuse a session** (`POST /api/session/{thread_key}`)

Creates a session for the thread, or returns the current one.

```
POST /api/session/slack%3AT123%3AC456%3A1773364194.179929
{
  "harness_type": "codex",
  "persona_id": "incident-responder",
  "metadata": {"platform": "slack"},
  "on_harness_conflict": "reject"
}

← {
    "thread_key": "slack:T123:C456:1773364194.179929",
    "sandbox_id": "sbx_123",
    "harness_type": "codex",
    "status": "active",
    "harness_switched": false
  }
```

**Step 2: Persist the user turn** (`POST /api/session/{thread_key}/messages`)

Writes one or more durable transcript messages. Parts use the same
Anthropic-style content block shape the sandbox adapter understands.

```
POST /api/session/slack%3AT123%3AC456%3A1773364194.179929/messages
{
  "messages": [
    {
      "client_message_id": "slack-evt-123",
      "role": "user",
      "parts": [{"type": "text", "text": "analyze this"}],
      "metadata": {"user_name": "alice", "platform": "slack"}
    }
  ]
}

← {"ok": true, "message_ids": ["msg_123"]}
```

**Step 3: Execute the session** (`POST /api/session/{thread_key}/execute`)

Creates a durable execution row and drives the attached sandbox. `input_lines`
are NDJSON strings sent to the harness adapter for this execution.

```
POST /api/session/slack%3AT123%3AC456%3A1773364194.179929/execute
{
  "idempotency_key": "slack-delivery-123",
  "metadata": {"platform": "slack"},
  "input_lines": [
    "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"analyze this\"}]}}"
  ]
}

← {
    "ok": true,
    "execution_id": "exe_123",
    "thread_key": "slack:T123:C456:1773364194.179929",
    "status": "queued"
  }
```

**Step 4: Stream or replay output** (`GET /api/session/{thread_key}/events`)

Consumers tail durable events for one execution. On disconnect, reconnect with
the last seen event id.

```
GET /api/session/slack%3AT123%3AC456%3A1773364194.179929/events?execution_id=exe_123&after_event_id=0

← SSE event: session.output.line
← data: {"type":"assistant","message":{...}}
← SSE event: session.execution_completed
← data: {"status":"completed","result_text":"..."}
```

**Inspect the active session context** (`GET /api/session/{thread_key}`)

Returns the normalized session context for a thread, including Slack channel
and thread timestamp information when the thread key is Slack-shaped.

**Durable state written for one turn:**

| Table | What |
|-------|------|
| `sessions` | Thread-to-sandbox assignment, harness, persona, and status |
| `session_messages` | Durable transcript messages |
| `session_executions` | Queued/running/terminal execution rows |
| `session_events` | Replayable execution, output, and status events |
| `session_warm_sandboxes` | SQL-backed warm-pool inventory and claims |

**API → Sandbox** (stdin/stdout, NDJSON):

api-rs communicates with sandbox Pods through the active sandbox backend's
attach stream. Execution `input_lines` are opaque newline-delimited strings at
the session API layer; api-rs validates that each item is one line, adds
session/trace context to JSON objects, writes them to sandbox stdin, and stores
each stdout line as a durable `session.output.line` event. Current chat clients
send Codex-compatible user lines shaped like:

```
→ stdin:  {"type":"user",
           "thread_key":"slack:T123:C456:1773364194.179929",
           "message":{
             "role":"user",
             "content":[
               {"type":"text","text":"what is this?"},
               {"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."}}
             ]
           ]}

← stdout: {"type":"system","subtype":"init","session_id":"T-..."}
← stdout: {"type":"assistant","message":{"role":"assistant","content":[...]}}
← stdout: {"type":"result","subtype":"success","result":"..."}
← stdout: {"type":"turn.completed","turn_id":"turn-1","result":"..."}
```

**Harness adapter behavior**:

The sandbox/runtime layer translates the user input content into whatever each
harness CLI actually accepts:

| Harness | Translation |
|---------|-------------|
| **claude-code** | Pass through directly (native Anthropic format) |
| **amp** | Materialize image/document blocks to files on disk, replace with `@/path` text mentions (Amp stdin only accepts text blocks) |
| **codex / pi-mono** | Extract text from content blocks, pass as CLI argument |

This means clients and api-rs avoid most harness-specific quirks. Clients send
durable messages plus the execution input lines they want delivered; the
sandbox/runtime adapter handles the target harness.

**Sandbox tools and API callbacks**:

Agent sandboxes do not use legacy HTTP tool-method routes as a registry.
Startup runs `services/sandbox/install_tool_shims.py`, which scans `TOOL_DIRS` for
`pyproject.toml [project.scripts]`, installs each script with `uvx`, and emits
the local `centaur-tools` catalog. Agents call tool CLIs directly; Python
workflow hosts can use the generated `centaur-tools call` bridge for
`ctx.call_tool(...)` compatibility.

### Network Isolation

The Helm chart installs deny-by-default NetworkPolicies, then explicitly allows
the service paths the stack needs: chat ingress services to api-rs, api-rs to
Postgres/iron-control/Kubernetes, sandbox Pods to api-rs/iron-proxy, DNS, and
configured egress.

## Directory Structure

```
centaur/
├── services/
│   ├── api-rs/           # Rust control plane, sessions, workflows, auth, metrics
│   │   ├── crates/centaur-api-server/
│   │   ├── crates/centaur-session-runtime/
│   │   ├── crates/centaur-session-sqlx/
│   │   ├── crates/centaur-workflows/
│   │   └── crates/centaur-perms/
│   ├── workflow-python/  # Python workflow host compatibility runtime
│   ├── iron-proxy/       # Credential injection proxy
│   ├── sandbox/          # Agent container image (Ubuntu 24.04 + uv + gh + node + bun + amp)
│   ├── slackbotv2/       # Slack event handling and Slack delivery
│   ├── teamsbot/         # Teams ingress
│   ├── discordbot/       # Discord ingress
│   ├── linearbot/        # Linear ingress
│   └── console/          # Admin/operator console
├── centaur_sdk/          # Standalone SDK (pip install centaur-sdk)
├── tools/                # Open-source tool plugins (auto-discovered)
│   ├── alchemy/          # One directory per tool — each has client.py + pyproject.toml
│   ├── websearch/
│   ├── telegram/
│   └── …                 # 60+ tool plugins (crypto, research, productivity, infra, …)
├── workflows/            # External workflow definitions (auto-discovered)
│   ├── agent_loop.py     # Recurring agent polling/monitoring loop
│   └── multi_step_demo.py       # Demo: branching, loops, conditionals
├── scripts/              # Operational scripts
└── Justfile              # Local Helm/Kubernetes workflow
```

## Terminology

- **Chat SDK** always refers to the [Vercel Chat SDK](https://github.com/vercel/chat) (`~/github/vercel/chat`). When you need to understand how the Chat SDK or `@chat-adapter/*` packages work, **always read the source at `~/github/vercel/chat`** — never dig through `node_modules`.

## Testing Before Pushing

**NEVER push changes without testing them locally first.** Testing means actually running the affected service and proving the change works end-to-end — not just linting or reasoning about it.

1. **Build the affected service:** `just build-one <service>`
2. **Bring it up:** `just deploy`
3. **Make a real request** that exercises the change and show the output
4. **Only then** commit and push

For tool changes: verify from a real sandbox with `centaur-tools list`,
`<tool> --help`, and a command that exercises the changed behavior. If the
change is only for workflow `ctx.call_tool(...)`, run a small workflow-host
workflow that calls it. For Dockerfile/infra changes: rebuild, redeploy, and
verify the binary/service is present and functional. For proxy changes: test
from inside a sandbox pod through iron-proxy.

## Local-First Testing — Never Touch the Deploy Box

**All testing and E2E validation MUST happen on the local Kubernetes stack** (`just up` on this machine).
The deploy box is **production**. Changes reach it via `git push` → GitHub Actions auto-deploy. The only reasons to SSH into it are:
- Checking logs (`kubectl logs`, VictoriaLogs queries) for debugging production issues
- Emergency manual intervention — **only when the user explicitly asks**

For E2E testing, always:
1. `just build-one <service>` locally
2. `just deploy` locally
3. Run curl commands against `localhost` through `kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl ...`
4. Verify results locally
5. Only then commit, push, and let CI/CD handle production

## Code Conventions

- Python 3.11+, `uv` for deps, `ruff` for lint/format (line-length=100)
- `services/slackbotv2` uses `pnpm` only (single lockfile: `pnpm-lock.yaml`)
- All imports at top of file, never inside functions
- Absolute imports only: `from api.X`, `from centaur_sdk.X`
- All secrets via env vars or secret manager, never hardcode
- `asyncpg` for Postgres, `pgvector` for embeddings
- Conventional commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`

## Lint & Test

Each service has its own `pyproject.toml` and `ruff.toml`. From the repo root:

```bash
uv run ruff check .          # lint
uv run ruff format .         # auto-fix
uv run pytest                # tests
```

## Plugin System — Tools & Workflows

Centaur has two plugin types that are auto-discovered at startup and hot-reloaded on file changes — no core code changes required to extend the system.

### Tool Plugins

Tools live in directories listed by `TOOL_DIRS` and ordered overlay sources.
Each tool is a directory with `client.py` (class + `_client()` factory),
`pyproject.toml`, and a CLI entry point exposed through `[project.scripts]`.
api-rs discovers tool metadata for secret grants; sandboxes install the
scripts as local CLI shims and list them with `centaur-tools list`.

- `client.py`: NO `load_dotenv()`. Secrets via `secret()` from `centaur_sdk.tool_sdk`.
- `cli.py`: YES `load_dotenv()` at top. Thin typer wrapper for standalone use.
- Methods starting with `_` are excluded from registration.
- Tool dependencies declared in `pyproject.toml` are installed by the shim
  runner when the script is installed.
- `[project.scripts]` is required for an agent-visible runtime tool.

Example:

```python
# tools/my-tool/client.py
import httpx

class MyToolClient:
    def search(self, query: str, limit: int = 10) -> dict:
        """Search for something."""
        resp = httpx.get(f"https://api.example.com/search?q={query}&limit={limit}")
        return resp.json()

def _client():
    return MyToolClient()
```

### Workflow Plugins

Workflows live in directories listed in the `WORKFLOW_DIRS` env var
(colon-separated paths). api-rs discovers workflow metadata through the Python
workflow host, and workflow-host sandboxes receive the same ordered list
translated to sandbox mount paths. Each workflow is a Python file exporting
`WORKFLOW_NAME`, an async `handler(params, ctx)`, and an optional `Input`
dataclass. See [Durable Workflows](#durable-workflows) for the full programming
model.

Built-in workflows ship in the top-level `workflows/` tree. External workflows
are loaded identically — add their directories to `WORKFLOW_DIRS` through the
ordered overlay configuration.

### Ordered Overlays

Centaur supports a first-class ordered overlay model, so organizations can extend the base repo without forking or relying on filesystem overlayfs. A common deployment keeps the base repo and an external overlay checkout side by side:

```
your-deployment/
├── centaur/              # This repo
└── centaur-overlay/      # Org-specific tools, workflows, skills, personas, prompt overlay
```

The Helm chart supports ordered overlays by mounting an overlay image or prompt content at `/app/overlay/org`, including its `tools/`, `workflows/`, `.agents/skills/`, persona prompts, and `services/sandbox/SYSTEM_PROMPT.md` after the base repo content.

Later overlay entries win cleanly when names collide, so the base repo stays generic while deployments can layer in org-specific behavior from outside the checkout.

## Durable Workflows

api-rs owns durable workflow state through Absurd queues, while
`services/workflow-python` runs Python workflow handlers in a workflow-host
sandbox. The Python compatibility layer exposes `WorkflowContext`, so the
handler function is still the workflow: steps are runtime-discovered via
`ctx.step(name, fn)`, checkpointed to Postgres, and skipped on replay after a
restart. Dynamic branching, loops, and conditional logic work naturally because
the handler remains Python.

### WorkflowContext API

Every handler receives `(params, ctx)` where `ctx: WorkflowContext` provides:

| Primitive | Purpose |
|-----------|---------|
| `ctx.step(name, fn)` | Execute *fn* exactly once; return cached result on replay. Supports `retry` (RetryPolicy) and `timeout`. |
| `ctx.sleep(name, duration)` | Suspend the run for *duration*; checkpoint + resume automatically. |
| `ctx.sleep_until(name, when)` | Suspend until a specific datetime. |
| `ctx.wait_for_event(name, event_type, correlation_id)` | Suspend until an external event arrives via `POST /api/workflows/events`. |
| `ctx.start_workflow(name, workflow_name, run_input)` | Create a child workflow run (returns immediately). |
| `ctx.wait_for_workflow(name, run_id)` | Suspend until a child workflow reaches terminal state. |
| `ctx.run_workflow(name, workflow_name, run_input)` | Start + wait in one call. |
| `ctx.start_agent(name, text=…)` | Shorthand: start a child `agent_turn` workflow. |
| `ctx.run_agent(name, text=…)` | Shorthand: start + wait for a child `agent_turn` workflow. |
| `ctx.log(msg, **kwargs)` | Structured log, suppressed during replay. |

### Writing a workflow

```python
# workflows/my_workflow.py
from dataclasses import dataclass
from typing import Any
from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "my_workflow"

@dataclass
class Input:
    message: str = "hello"

async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    greeting = await ctx.step("gather", lambda: {"msg": inp.message})
    await ctx.sleep("pause", timedelta(minutes=5))
    result = await ctx.run_agent("agent", text=f"Summarize: {greeting['msg']}")
    return {"greeting": greeting, "agent_result": result}
```

### Workflow lifecycle

Runs go through: `queued → running → sleeping/waiting → running → … → completed/failed/cancelled`.

- **Worker pool**: `WORKFLOW_WORKER_CONCURRENCY` workers (default 2) poll for claimable runs.
- **Lease-based fencing**: Each worker holds a lease on its run, extended by a heartbeat. If the worker dies, the lease expires and another worker reclaims the run.
- **Schedules**: Cron-based or interval-based schedules are discovered from workflow metadata by `api-rs`. The scheduler stores tick tasks in the Absurd `centaur_workflow_schedules` queue.
- **External events**: `POST /api/workflows/events` delivers events that wake waiting runs.
- **Child workflows**: Parent→child relationships are tracked; cancelling a parent cancels linked executions.

### Workflow REST API

| Endpoint | Purpose |
|----------|---------|
| `POST /api/workflows/runs` | Create a workflow run (`workflow_name`, `input`, optional idempotency fields, `eager_start`) |
| `GET /api/workflows/runs` | List recent runs. |
| `GET /api/workflows/runs/{run_id}` | Get run details. |
| `POST /api/workflows/runs/{run_id}/cancel` | Cancel a run. |
| `POST /api/workflows/events` | Deliver an external event (`event_name`, `payload`). |
| `GET /api/workflows/schedules` | Inspect registered workflow schedules. |

### Built-in workflows

| Workflow | Description |
|----------|-------------|
| `echo` | Minimal smoke workflow. |
| `slack_sync`, `slack_backfill` | Slack ETL sync and backfill jobs. |
| `company_context_documents` | Projection from synced sources into retrieval documents. |
| `google_drive_sync`, `google_calendar_sync`, `linear_sync` | Optional connector sync workflows. |
| `github_issue_triage` | Example webhook-triggered triage flow. |

### Durable state

| Table | What |
|-------|------|
| `absurd.queues` | Registered workflow queues, including standard, ETL, backfill, Slack-live, and schedule queues |
| `absurd.t_centaur_workflows*` | Workflow task metadata, state, input params, idempotency keys, and completed payloads |
| `absurd.r_centaur_workflows*` | Per-attempt run state, leases, timing, result payloads, and failures |
| `absurd.c_centaur_workflows*` | Per-step checkpoint state |
| `absurd.e_centaur_workflows*` | Emitted workflow events for event-driven resumes |
| `absurd.w_centaur_workflows*` | Wait registrations for sleeps and external events |
| `absurd.t_centaur_workflow_schedules` | Scheduler tick tasks for registered cron and interval schedules |

## Agent Sandbox

### Overview

1 conversation = 1 Kubernetes sandbox Pod. api-rs spawns Pods running harness
CLIs (amp, claude-code, codex), streams messages over the sandbox attach
channel, and records output in durable session events.

### How the System Prompt Works

The sandbox image bakes `services/sandbox/SYSTEM_PROMPT.md` into `~/AGENTS.md` at build time. On container startup, `entrypoint.sh` copies it into the workspace root as `workspace/AGENTS.md` — this is the file that AI harnesses (Amp, Claude Code, Codex) read as their system instructions.

The system prompt tells the agent:
- **Identity**: it's running inside a Kubernetes sandbox pod managed by api-rs
- **Tools**: three kinds — harness built-ins (Read, Bash, etc.), tool plugins exposed as shell CLI shims, and a headless browser
- **Tool CLIs**: each tool is installed as a shell command at container startup by `services/sandbox/install_tool_shims.py`, which scans `TOOL_DIRS` for `pyproject.toml [project.scripts]` and `uvx`-installs each. Agents invoke tool CLIs directly (`slack get_channel_history '{"channel":"general"}'`, `<tool> --help` to discover).
- **Slack messaging**: the agent's stdout IS the Slack reply — never call `send_message` on the active thread
- **Rules**: never display secrets, show your work, lead with the answer

`centaur-tools` is the generated catalog CLI emitted by the same installer:
- `centaur-tools list` → list available tool CLIs
- `centaur-tools run <tool> [args]` → run a tool CLI
- `centaur-tools call <tool> <method> [json]` → internal compatibility for the Python workflow host's `ctx.call_tool(...)`
- `<tool> --help` → discover one tool's direct CLI

### Persona System

The entrypoint supports persona overlays via `AGENT_PERSONA`. Persona prompts are discovered from the loaded tool directories (including overlays such as `~/centaur-overlay`) and appended after the base + org overlay system prompts at container startup.

### Sandbox Pod Config

- Runs under Kubernetes NetworkPolicies with API reachable through the in-cluster service URL
- Entrypoint injects the runtime URLs and tool catalog environment needed by the sandbox
- Stub API keys so harnesses init in API-key mode (not browser login)
- `HTTPS_PROXY` routes LLM and tool egress through iron-proxy
- Resource limits: 4GB memory, 2 CPUs
- Image tagged `centaur-agent:latest`
- Labels identify Centaur-managed sandboxes and carry thread/harness metadata for discovery/recovery

### Credential Injection (iron-proxy)

Sandbox Pods never see real API keys. Per-sandbox `iron-proxy` pods inject
credentials from the configured secret source and iron-control grants:

| Target host | Header | Format |
|-------------|--------|--------|
| `api.anthropic.com` | `x-api-key` | raw |
| `api.openai.com` | `authorization` | bearer |
| `openrouter.ai` | `authorization` | bearer |
| `ampcode.com` | `authorization` | bearer |
| `api.github.com` | `authorization` | token |
| `github.com` | `authorization` | basic auth |
| `bedrock-mantle.<region>.api.aws` | `authorization` + `x-amz-*` | AWS SigV4 re-sign (opt-in, codex `amazon-bedrock`) |

### Session Persistence

- **`sessions`** table: tracks thread key, sandbox ID, harness, persona, and state
- **`session_messages`** table: stores persisted user/assistant messages
- **`session_executions`** and **`session_events`** tables: store durable run state and replayable output
- On api-rs restart, sandbox ownership is re-read from Postgres; process-local attach pipes are rebuilt lazily per sandbox
- Pods are still discoverable via Kubernetes labels even if DB state needs reconciliation

## Security Model

- **API auth**: Chat ingress services use deployment-scoped bearer tokens such as `SLACKBOT_API_KEY`, `TEAMSBOT_API_KEY`, `DISCORDBOT_API_KEY`, or `LINEARBOT_API_KEY` when configured. Local in-cluster service calls use the internal api-rs service URL.
- **Sandbox auth**: Sandbox Pods use the runtime's tool and workflow surfaces; agents should not depend on a user-visible Centaur API key.
- **Slack**: HMAC-SHA256 signature verification on all webhooks
- **Public edge**: The Helm chart exposes public routes only when configured through Ingress, HTTPRoute, or service settings.
- **Sandbox isolation**: Pods get stub keys only; real keys are injected by iron-proxy in-flight
- **Filesystem**: Host repos mounted read-only by default; only working repo is read-write
- **Kubernetes API**: The API service account is scoped to the Pod, Secret, exec, attach, and log operations needed to manage sandboxes.

## API Key Management

Chat ingress services send bearer tokens from the local infra Secret when the
deployment configures them. The current api-rs control plane does not use the
legacy DB-backed API-key table or legacy key prefix for the session routes.

### Key types

| Type | Prefix | Issued by | Used by | Scopes |
|------|--------|-----------|---------|--------|
| Service bearer | deployment-specific | Kubernetes Secret / bootstrap | Slackbotv2, Teamsbot, Discordbot, Linearbot | Service-to-api-rs calls |

### How services get their keys

- **Slackbotv2**: `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET`, and `SLACKBOT_API_KEY` are injected from the local infra Secret.
- **Sandbox containers**: Use runtime-provided tool CLIs and workflow context rather than a direct Centaur API key
- **Local testing**: Exec into the api-rs deployment and use `localhost:8080`, or use a bot/service token path configured for that deployment

## Secrets

Tool credentials (e.g., `ANTHROPIC_API_KEY`, `AMP_API_KEY`) are never materialized inside sandboxes or the API service. Tools declare which keys they need in their `pyproject.toml` and call `secret("KEY")` to receive a placeholder. Outbound HTTPS traffic is routed through iron-proxy, which substitutes the real credential based on the host/key injection map and iron-control grants. iron-proxy resolves `op://...` references directly against 1Password when that source is configured.

For local development, infra secrets are stored in Kubernetes Secrets created by `just bootstrap-secrets`; application secrets continue to come from 1Password.

### iron-control

[iron-control](https://github.com/ironsh/iron-control) is an optional Rails control plane for permissioning and encrypted secret storage. It is off by default; enable it with `--set ironControl.enabled=true` (or set `ironControl.enabled: true` in a values file). When enabled, it runs against a dedicated `iron_control_production` database on the bundled Postgres (a separate logical DB so its Rails `schema_migrations` table never collides with api-rs SQLx migrations), created by an idempotent init container.

`just bootstrap-secrets` seeds the required keys into `centaur-infra-env`: the three ActiveRecord encryption keys, `SECRET_KEY_BASE`, and the initial admin password/API key are auto-generated (only when absent, never rotated in place). `IRON_CONTROL_DATABASE_URL` defaults to the bundled Postgres server with no database path (so Rails resolves each connection's database name from the image's `database.yml`); export it before running `just bootstrap-secrets` to point at an external server. Override the admin email with `IRON_CONTROL_INITIAL_USER_EMAIL` (default `admin@centaur.local`).

### centaur-perms

`centaur-perms` is the operator CLI for iron-control permissions: it controls which chat principals (Slack users/channels, Discord channels, and Teams users/conversations) and which roles hold which tool roles and secrets. It lives at `services/api-rs/crates/centaur-perms` and reuses iron-control's canonical mappings (`derive_principal`, `RoleSpec::tool`), so every principal and role `foreign_id` it writes matches exactly what `api-rs` registers at session start. It is the supported way to inspect and edit grants by hand; the API writes the same resources at runtime.

#### Concepts

- **Principal** — the chat identity an agent session runs as. `foreign_id`s are derived canonically: `slack-channel-<team>-<conv>` for a Slack channel, `slack-user-<team>-<user>` for a Slack DM, `discord-channel-<guild>-<channel>` for Discord, `teams-conversation-<tenant-slug>-<conversation-slug>` for Teams conversations, and `teams-user-<tenant-slug>-<user-slug>` for Teams user-scoped runs. Each session binds to one derived principal; grant the channel/conversation principal for shared contexts and the user principal for DM or user-scoped contexts.
- **Role** — a named bundle of secret grants assignable to principals. Canonical roles: `infra` (shared infra secrets), `tools` (shared harness/tool secrets), and one `tool-<slug>` per tool (e.g. `tool-github`).
- **Secret** — a typed iron-control resource (static `ssr_`, OAuth token `ots_`, GCP auth `gas_`, Postgres DSN `pgs_`, HMAC signing `hms_`). iron-control never returns credential values, only the source each resolves from. Each `tool-<slug>` secret keeps a canonical `tool-<slug>-…` id so the same object is shared no matter which role grants it.
- **Grant** — binds a secret to a grantee (a principal or a role). `centaur-perms` resources carry the label `managed-by=centaur`.

A principal's *effective* access is the union of its directly granted secrets and the secrets carried by every role assigned to it.

#### Setup

The CLI talks to the iron-control admin API. Provide the connection via flags or env vars (iron-control must be enabled — see above):

```bash
export IRON_CONTROL_URL=http://localhost:3000        # admin API base URL
export IRON_CONTROL_API_KEY=iak_…                    # admin API key
export IRON_CONTROL_NAMESPACE=default                # optional, defaults to "default"
```

For `--tool` lookups, point the CLI at the same tool directories the API uses, via repeatable `--tools-dir` flags or the colon-separated `TOOL_DIRS` env var (explicit dirs first, then env; later dirs shadow earlier ones, matching the overlay order). Build and run from `services/api-rs`:

```bash
cd services/api-rs
cargo run -p centaur-perms -- <args>     # or: cargo build -p centaur-perms; ./target/debug/centaur-perms <args>
```

The `--tool` flag parses a tool's `pyproject.toml` `[tool.centaur]` secrets and registers them in iron-control before granting. How each secret's `secret_ref` resolves to a source is set by `--source-policy` (`env` default, `onepassword`, or `onepassword-connect`); the 1Password policies also require `--op-vault` (and accept `--op-ttl`, default `10m`).

#### Command surface

Commands are resource-first — `centaur-perms <noun> <verb>`:

| Command | What it does |
|---------|--------------|
| `principals list [--filter S] [--label k=v] [--managed]` | List principals. `--filter` is a case-insensitive substring on `foreign_id`/name; `--managed` is shorthand for `--label managed-by=centaur`. |
| `principals show <principal> [--slack-user U]` | Show a principal's roles (with each role's grants), direct grants, and effective replace-secret placeholders. |
| `principals grant <principal> [--slack-user U] [--tool N] [--role F] [--secret OID]` | Grant access. `--tool` registers its `tool-<slug>` role + secrets then assigns it; `--role` assigns an existing role; `--secret` grants a secret OID directly. All repeatable; creates the principal if absent. |
| `principals revoke <principal> [--slack-user U] [--tool N] [--role F] [--secret OID] [--grant-id OID]` | Reverse of grant. `--tool`/`--role` unassign the role; `--secret` deletes the direct grant for that secret; `--grant-id` deletes a grant by its `grant_…` id. |
| `roles list / show <role>` | List roles, or show the secrets granted to one role. |
| `roles grant <role> [--secret OID] [--tool N [--secret-name NAME]]` | Grant secrets to a role by OID, or register+grant a tool's declared secrets. `--secret-name` (repeatable, requires `--tool`) selects specific declared secrets instead of all. |
| `roles revoke <role> --secret OID` | Revoke one or more secrets from a role (`--secret` required, repeatable). |
| `secrets list [--filter S] [--label k=v] [--managed]` | List secrets across every type, one row per secret. |
| `secrets show <secret>` | Show one secret's full config by OID or `foreign_id` (values are never shown — only the source). |
| `broker create --foreign-id F --token-endpoint URL --client-id ID [--client-secret S] [--refresh-token SEED] [--scope SC]…` | Create or update an iron-control broker credential. Values are passed literally; iron-control owns the OAuth refresh loop. Re-supplying `--refresh-token` re-bootstraps it. |
| `broker list / show <credential> / delete <credential>` | List broker credentials, show one (status/expiry; secret material is never returned), or delete one (by `bcr_` OID or `foreign_id`). |

A `<principal>` argument is treated as a chat thread key when it contains `:` (for example `slack:T123:C456:1700000000.0001`, `discord:111:222:333`, or `teams:<base64url-conversation-id>:<base64url-service-url>`) and run through `derive_principal`. Pass `--slack-user` so a Slack DM thread keys to the user. Any value without a `:` is used verbatim as a `foreign_id` (e.g. `slack-channel-t123-c456` or `teams-conversation-19-abc123-thread-tacv2`) or an OID. Grant/revoke operations are idempotent: re-granting an assigned role or revoking a missing grant is a no-op, reported as such.

A tool's `brokered_token` secret registers the *consumer* side — a static secret that injects the access token from a `token_broker` source. The broker credential itself (the managed OAuth refresh loop) is provisioned out of band with `broker create`; the tool's `brokered_token` references it by `foreign_id` (its `credential`, defaulting to the secret `name`).

#### Common workflows

Give a channel access to a tool (registers the tool's role + secrets from its `pyproject.toml`, then assigns the role to the channel):

```bash
centaur-perms principals grant slack-channel-t123-c456 --tool github --tools-dir tools
```

Inspect what a principal can actually do (resolve a live thread key, then list roles, direct grants, and effective secrets):

```bash
centaur-perms principals show slack:T123:C456:1700000000.0001
```

Give an individual user a tool only in their DMs:

```bash
centaur-perms principals grant slack:D9999999:1700000000.0001 --slack-user U07ABC --tool github --tools-dir tools
```

Register a tool's secrets once on the shared `tools` role, then assign that role to many principals:

```bash
centaur-perms roles grant tools --tool github --tools-dir tools
centaur-perms principals grant slack-channel-t123-c456 --role tools
```

Register only a single named secret from a tool onto a role:

```bash
centaur-perms roles grant infra --tool slackbot --secret-name SLACK_BOT_TOKEN --tools-dir tools
```

Revoke a tool from a channel (unassigns the `tool-<slug>` role; shared secrets on other roles are untouched):

```bash
centaur-perms principals revoke slack-channel-t123-c456 --tool github
```

Provision a managed broker credential a `brokered_token` secret (or a harness fragment) references — e.g. the Codex/Claude Code access-token harnesses reference `openai-codex` / `anthropic-claude`:

```bash
centaur-perms broker create --foreign-id openai-codex \
  --token-endpoint https://auth.openai.com/oauth/token \
  --client-id "$OPENAI_CODEX_CLIENT_ID" --refresh-token "$OPENAI_CODEX_REFRESH_TOKEN"
```

Audit Centaur-managed secrets and inspect one:

```bash
centaur-perms secrets list --managed
centaur-perms secrets show tool-github-github_token
```

## Observability & Audit Logs

### Architecture

All services write structured JSON logs to **stdout**. Kubernetes captures pod logs, and optional observability deployments can forward them to VictoriaLogs. api-rs exposes Prometheus metrics at `/metrics` when scraping is enabled.

```
Service → stdout (JSON) → Kubernetes pod logs → optional log collector → VictoriaLogs/Grafana
```

This design keeps the local Helm stack minimal while preserving structured logs for collectors.

### Components

| Component | Role | Config |
|-----------|------|--------|
| **VictoriaLogs** | Optional log storage + query engine | External/overlay deployment |
| **VictoriaMetrics** | Optional metrics storage + query engine | Push-based when enabled |
| **Grafana** | Optional dashboards + log explorer | External/overlay deployment |

### Querying logs

Via Grafana: navigate to **Explore → VictoriaLogs** and use [LogsQL](https://docs.victoriametrics.com/victorialogs/logsql/).

Via CLI (from inside the Kubernetes network):

```bash
# All logs for a specific thread
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s "http://victorialogs:9428/select/logsql/query" \
  --data-urlencode "query=thread_key:C042WDDP89Y" --data-urlencode "limit=50"

# API errors in the last hour
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s "http://victorialogs:9428/select/logsql/query" \
  --data-urlencode "query=_stream:{service=\"api-rs\"} AND level:error" --data-urlencode "limit=20"

# Firewall audit trail for a time range
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s "http://victorialogs:9428/select/logsql/query" \
  --data-urlencode "query=_stream:{service=\"iron-proxy\"} AND event:proxy_audit" \
  --data-urlencode "start=2026-03-10T00:00:00Z" --data-urlencode "end=2026-03-11T00:00:00Z"
```

### Audit logging

**iron-proxy** emits structured audit events for outbound requests from sandbox
containers: method, host, path, status code, request/response bytes, duration,
and source container IP. These are searchable via `event:proxy_audit` in
VictoriaLogs.

**api-rs** logs session lifecycle, workflow, sandbox, proxy, and HTTP request
events with thread context.

### Logging contract

Services must write single-line JSON to stdout with these fields:

| Field | Required | Description |
|-------|----------|-------------|
| `timestamp` | Yes | ISO 8601 timestamp |
| `level` | Yes | `debug`, `info`, `warning`, `error` |
| `service` | Yes | Service name (`api-rs`, `iron-proxy`, `slackbotv2`, etc.) |
| `event` | Yes | Machine-readable event name |
| `msg` | No | Human-readable message |
| `thread_key` | No | Thread identifier (when applicable) |

> **Never log secret values, auth headers, or raw tokens.**

## E2E Testing (without Slack)

### 1. Bring up the stack

```bash
just up
```

All E2E curl commands below use `kubectl exec` for localhost bypass (no API key needed).
To test from outside the container, create a DB-backed key via the [admin API](#api-key-management).

### 2. Create or reuse a session

```bash
THREAD_KEY=cli:test-e2e-1
THREAD_PATH=$(jq -rn --arg v "$THREAD_KEY" '$v|@uri')

SESSION=$(kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}" \
  -H "Content-Type: application/json" \
  -d '{"harness_type":"codex","on_harness_conflict":"restart"}')
```

### 3. Persist a message

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}/messages" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","parts":[{"type":"text","text":"Reply with exactly PONG and nothing else."}]}]}'
```

### 4. Execute the session

```bash
EXECUTE=$(kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}/execute" \
  -H "Content-Type: application/json" \
  -d '{"input_lines":["{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG and nothing else.\"}]}}"]}')
EXECUTION_ID=$(printf '%s' "$EXECUTE" | jq -r '.execution_id')
```

### 5. Tail durable events (or reconnect later)

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -N \
  "http://localhost:8080/api/session/${THREAD_PATH}/events?execution_id=${EXECUTION_ID}&after_event_id=0"
```

If this stream disconnects, reconnect with the last seen SSE `id` as
`after_event_id`.

### 6. Inspect the session

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s \
  "http://localhost:8080/api/session/${THREAD_PATH}" | jq
```

### Debugging

```bash
kubectl get pods -n centaur -l centaur.ai/managed=true
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s http://localhost:8080/healthz
kubectl exec -n centaur <sandbox-pod> -- centaur-tools list
```
