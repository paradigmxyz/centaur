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

```bash
./scripts/dbmate new add_agent_leases
./scripts/dbmate --set overlay new add_org_tables
./scripts/dbmate status
./scripts/dbmate up
```

`./scripts/dbmate` creates the next numbered SQL file in `services/api/db/migrations` by default, or in `services/api/db/migrations` inside the mounted overlay when you pass `--set overlay`. `up`, `migrate`, and `status` run against both the core and overlay migration sets unless you pin a specific set. Each set has its own dbmate migrations table so overlay repos can extend the shared Postgres database without version collisions. If `DATABASE_URL` is not set in your shell, the wrapper reuses the API deployment's configured value through `kubectl exec`.

### 3. Test

From inside the API deployment (localhost bypass — no key needed):

```bash
THREAD_KEY=test-e2e-1

SPAWN=$(kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST http://localhost:8000/agent/spawn \
  -H "Content-Type: application/json" \
  -d "{\"thread_key\":\"${THREAD_KEY}\",\"harness\":\"amp\"}")
ASSIGNMENT_GENERATION=$(printf '%s' "$SPAWN" | jq -r '.assignment_generation')

kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST http://localhost:8000/agent/message \
  -H "Content-Type: application/json" \
  -d "{\"thread_key\":\"${THREAD_KEY}\",\"assignment_generation\":${ASSIGNMENT_GENERATION},\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG and nothing else.\"}]}"

EXECUTE=$(kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST http://localhost:8000/agent/execute \
  -H "Content-Type: application/json" \
  -d "{\"thread_key\":\"${THREAD_KEY}\",\"assignment_generation\":${ASSIGNMENT_GENERATION},\"harness\":\"amp\",\"delivery\":{\"platform\":\"dev\"}}")
EXECUTION_ID=$(printf '%s' "$EXECUTE" | jq -r '.execution_id')

kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s "http://localhost:8000/agent/executions/${EXECUTION_ID}" | jq
```

Or create a DB-backed key for external use (see [API Key Management](#api-key-management)).

## Architecture

See the [architecture diagram in the README](README.md#architecture).

### End-to-End Request Flow

1. User mentions bot in Slack → webhook → slackbot → api
2. API spawns/reuses a Kubernetes sandbox pod (`centaur-agent:latest`) for that thread
3. Executes harness (amp/claude-code/codex) through the sandbox backend
4. Harness calls tools via `curl` back to API at `http://api:8000` (REST, NOT MCP)
5. LLM API calls route through firewall proxy which injects real credentials
6. Results stream as JSON events → posted to Slack

### Service Interface Contracts

Centaur is a modular service architecture. Each service communicates through well-defined interfaces. As long as you implement these interfaces, you can swap or extend any layer independently.

**Client → API** (durable control-plane protocol):

Clients (slackbot, CLI, external integrations) should stay thin. They persist input with `spawn -> message -> execute`, stream or replay output from the durable events endpoint, and only fall back to durable terminal state when the live stream is gone. The API owns runtime assignment, execution serialization, cancellation, and final-delivery recovery; Postgres is the source of truth.

**Step 1: Assign or reuse a runtime** (`POST /agent/spawn`)

Pins one warm runtime to the thread and returns the current `assignment_generation`.

```
POST /agent/spawn
{
  "thread_key": "slack:C0AJ07U8Z1N:1773364194.179929",
  "harness": "amp"
}

← {
    "thread_key": "slack:C0AJ07U8Z1N:1773364194.179929",
    "runtime_id": "rtm_123",
    "assignment_generation": 12,
    "state": "assigned_idle"
  }
```

**Step 2: Persist the user turn** (`POST /agent/message`)

Writes one durable transcript event. Inline base64 image/document blocks are extracted into `attachments` and rewritten to lightweight `attachment_ref` parts.

```
POST /agent/message
{
  "thread_key": "slack:C0AJ07U8Z1N:1773364194.179929",
  "assignment_generation": 12,
  "role": "user",
  "parts": [{"type": "text", "text": "analyze this"}],
  "user_id": "U123",
  "metadata": {"user_name": "alice", "platform": "slack"}
}

← {"ok": true, "message_id": "msg_123"}
```

**Step 3: Enqueue execution** (`POST /agent/execute`)

Creates a durable execution request plus final-delivery obligation. The worker drives the attached container; the response is just the execution handle.

```
POST /agent/execute
{
  "thread_key": "slack:C0AJ07U8Z1N:1773364194.179929",
  "assignment_generation": 12,
  "harness": "amp",
  "delivery": {"platform": "slack"}
}

← {"ok": true, "execution_id": "exe_123", "status": "queued"}
```

**Step 4: Stream or replay output** (`GET /agent/threads/{thread_key}/events`)

Consumers tail durable events for one execution. On disconnect, reconnect with the last seen event id. If the execution already finished and no more rows remain, the API emits the terminal `execution_state` snapshot.

```
GET /agent/threads/slack:C0AJ07U8Z1N:1773364194.179929/events?execution_id=exe_123&after_event_id=0

← SSE event: amp_raw_event
← data: {"type":"assistant","message":{...}}
← SSE event: turn.done
← data: {"type":"turn.done","result":"..."}
← SSE event: execution_state
← data: {"status":"completed","result_text":"..."}
```

**Step 5: Release only when you really want to end the assignment** (`POST /agent/threads/{thread_key}/release`)

Releases the thread-to-runtime pin and optionally cancels any non-terminal execution still tied to that assignment generation.

**Inspect the active runtime for a thread** (`GET /agent/runtime?key={thread_key}`)

Returns `{persona_id, persona, harness, engine, overlay: {loaded, mount_api, mount_sandbox, image}, available_personas, …}`. Sandboxes call this through `call agent runtime '?key='"$CENTAUR_THREAD_KEY"`; clients can call it directly to confirm what persona/overlay an assignment is actually running.

**Durable state written for one turn:**

| Table | What |
|-------|------|
| `agent_runtime_assignments` | Thread-to-runtime pin and active assignment generation |
| `agent_message_requests` | Durable inbound transcript events |
| `attachments` | Extracted attachment bytes for inline multimodal content |
| `agent_execution_requests` | Queued/running/terminal execution row |
| `agent_execution_events` | Replayable raw + projected execution events |
| `agent_final_delivery_outbox` | Final-result delivery obligation for reconnect/retry paths |

`POST /agent/connect` and `POST /agent/reconnect` are legacy endpoints now kept only as explicit `410 LEGACY_ENDPOINT_REMOVED` stubs. Do not build new clients on them.

**API → Sandbox** (stdin/stdout, NDJSON):

The API communicates with sandbox Pods through the active sandbox backend's attach stream. The wire format is **Anthropic message format** — this is the canonical protocol between the API and all sandboxes, regardless of which harness runs inside.

```
→ stdin:  {"type":"turn.start","turn_id":1,"text":"analyze this"}
→ stdin:  {"type":"turn.start","turn_id":2,"content":[             // Anthropic content blocks
             {"type":"text","text":"what is this?"},
             {"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."}}
           ]}
→ stdin:  {"type":"interrupt"}

← stdout: {"type":"system","subtype":"init","session_id":"T-..."}
← stdout: {"type":"assistant","message":{"role":"assistant","content":[...]}}
← stdout: {"type":"result","subtype":"success","result":"..."}
← stdout: {"type":"turn.done","turn_id":1,"result":"..."}
```

**Sandbox harness adapter** (`services/sandbox/harness_session.py`):

The sandbox's `harness_session.py` translates the standard Anthropic format into whatever each harness CLI actually accepts:

| Harness | Translation |
|---------|-------------|
| **claude-code** | Pass through directly (native Anthropic format) |
| **amp** | Materialize image/document blocks to files on disk, replace with `@/path` text mentions (Amp stdin only accepts text blocks) |
| **codex / pi-mono** | Extract text from content blocks, pass as CLI argument |

This means clients and the API never need to know about harness-specific quirks. They speak Anthropic format; the sandbox adapter handles the rest.

**Sandbox → API** (REST over Kubernetes services):

Agents call tools through the generated `centaur-tools` catalog and direct tool CLIs. The tool runtime handles routing and credential access.

### Network Isolation

The Helm chart installs deny-by-default NetworkPolicies, then explicitly allows the service paths the stack needs: Slackbot to API, API to Postgres/secrets/firewall/Kubernetes, sandbox Pods to API/firewall, DNS, and configured egress.

## Directory Structure

```
centaur/
├── services/
│   ├── api/              # FastAPI control plane (standalone service)
│   │   ├── api/          # Python package
│   │   │   ├── routers/  # HTTP endpoints (agent, workflows, admin, health, …)
│   │   │   ├── sandbox/  # Sandbox backend abstraction (Kubernetes)
│   │   │   ├── workflows/# Built-in workflow handlers (agent_turn, slack_thread_turn)
│   │   │   ├── runtime_control.py   # Durable execution control-plane
│   │   │   ├── workflow_engine.py   # Durable workflow engine (checkpoint/replay)
│   │   │   ├── warm_pool.py         # Pre-warmed sandbox pool
│   │   │   ├── vm_metrics.py        # Push-based VictoriaMetrics metrics
│   │   │   └── observability.py     # Execution observation projections
│   │   ├── Dockerfile
│   │   └── tools.toml    # Tool plugin directory config
│   ├── secrets/          # Pluggable secrets manager (standalone service)
│   ├── firewall/         # mitmproxy addon — credential injection proxy
│   ├── sandbox/          # Agent container image (Ubuntu 24.04 + uv + gh + node + bun + amp)
│   ├── slackbot/         # Next.js + Slack Bolt event listener (pnpm)
│   ├── grafana/          # Grafana dashboards + provisioning
│   ├── fluentbit/        # Fluent Bit log shipping config
│   └── alloy/            # Grafana Alloy config
├── centaur_sdk/          # Standalone SDK (pip install centaur-sdk)
├── packages/             # Shared packages (api-client, harness-events)
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

For tool changes: tools hot-reload, so just verify via `curl -X POST http://localhost:8000/tools/<tool>/<method>` from inside the API deployment. For Dockerfile/infra changes: rebuild, redeploy, and verify the binary/service is present and functional. For firewall changes: test from inside a sandbox pod through the proxy.

## Local-First Testing — Never Touch the Deploy Box

**All testing and E2E validation MUST happen on the local Kubernetes stack** (`just up` on this machine).
The deploy box is **production**. Changes reach it via `git push` → GitHub Actions auto-deploy. The only reasons to SSH into it are:
- Checking logs (`kubectl logs`, VictoriaLogs queries) for debugging production issues
- Emergency manual intervention — **only when the user explicitly asks**

For E2E testing, always:
1. `just build-one <service>` locally
2. `just deploy` locally
3. Run curl commands against `localhost` through `kubectl exec -n centaur deploy/centaur-centaur-api -- curl ...`
4. Verify results locally
5. Only then commit, push, and let CI/CD handle production

## Code Conventions

- Python 3.11+, `uv` for deps, `ruff` for lint/format (line-length=100)
- `services/slackbot` uses `pnpm` only (single lockfile: `pnpm-lock.yaml`)
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

Tools live in directories listed in `tools.toml` (`plugin_dirs`). Each tool is a directory with `client.py` (class + `_client()` factory), `pyproject.toml`, and optional `cli.py`. The API auto-discovers tools on startup, generates REST endpoints at `/tools/{name}/{method}`, and hot-reloads on file changes.

- `client.py`: NO `load_dotenv()`. Secrets via `secret()` from `centaur_sdk.tool_sdk`.
- `cli.py`: YES `load_dotenv()` at top. Thin typer wrapper for standalone use.
- Methods starting with `_` are excluded from registration.
- Tool dependencies declared in `pyproject.toml` are installed at image build time.

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

Workflows live in directories listed in the `WORKFLOW_DIRS` env var (colon-separated paths, bind-mounted into the API container). Each workflow is a single Python file exporting `WORKFLOW_NAME`, an async `handler(params, ctx)`, and an optional `Input` dataclass. See [Durable Workflows](#durable-workflows) for the full programming model.

Built-in workflows ship in `services/api/api/workflows/`. External workflows (like those in the top-level `workflows/` directory) are loaded identically — just point `WORKFLOW_DIRS` at them.

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

The workflow engine (`workflow_engine.py`) provides a checkpoint/replay model inspired by [Cloudflare Workflows](https://developers.cloudflare.com/workflows/). The handler function IS the workflow — steps are runtime-discovered via `ctx.step(name, fn)` calls. The engine checkpoints each step result to Postgres. On resume after crash or suspension, the handler re-executes top-to-bottom but skips steps that already have checkpoints (returning the cached result instantly). Dynamic branching, loops, and conditional logic work naturally because it is just Python.

### WorkflowContext API

Every handler receives `(params, ctx)` where `ctx: WorkflowContext` provides:

| Primitive | Purpose |
|-----------|---------|
| `ctx.step(name, fn)` | Execute *fn* exactly once; return cached result on replay. Supports `retry` (RetryPolicy) and `timeout`. |
| `ctx.sleep(name, duration)` | Suspend the run for *duration*; checkpoint + resume automatically. |
| `ctx.sleep_until(name, when)` | Suspend until a specific datetime. |
| `ctx.wait_for_event(name, event_type, correlation_id)` | Suspend until an external event arrives via `POST /workflows/events`. |
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
- **External events**: `POST /workflows/events` delivers events that wake waiting runs.
- **Child workflows**: Parent→child relationships are tracked; cancelling a parent cascels linked executions.

### Workflow REST API

| Endpoint | Purpose |
|----------|---------|
| `POST /workflows/runs` | Create a workflow run (`workflow_name`, `input`, optional `trigger_key` for idempotency, `eager_start`) |
| `GET /workflows/runs` | List runs (filter by `workflow_name`, `thread_key`, `status`, `parent_run_id`) |
| `GET /workflows/runs/{run_id}` | Get run details (status, checkpoints, waiting_on) |
| `GET /workflows/runs/{run_id}/children` | List child workflow runs |
| `GET /workflows/runs/{run_id}/checkpoints` | Inspect all checkpoints for a run |
| `POST /workflows/runs/{run_id}/cancel` | Cancel a run (idempotent for terminal runs) |
| `POST /workflows/events` | Deliver an external event (`event_type`, `correlation_id`, `payload`) |

### Built-in workflows

| Workflow | Description |
|----------|-------------|
| `agent_turn` | Single durable agent turn: spawn → message → execute → wait for terminal result. |
| `slack_thread_turn` | Same as `agent_turn` but requires a Slack `thread_key`. Used by the slackbot. |
| `agent_loop` | Recurring agent loop: runs an agent turn every N seconds until the agent signals `{"done": true}`, max iterations, or deadline. |

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

1 conversation = 1 Kubernetes sandbox Pod. The API spawns Pods running harness CLIs (amp, claude-code, codex). Inside the Pod, the harness calls back to the API via `curl` over REST.

### How the System Prompt Works

The sandbox image bakes `services/sandbox/SYSTEM_PROMPT.md` into `~/AGENTS.md` at build time. On container startup, `entrypoint.sh` copies it into the workspace root as `workspace/AGENTS.md` — this is the file that AI harnesses (Amp, Claude Code, Codex) read as their system instructions.

The system prompt tells the agent:
- **Identity**: it's running inside a Kubernetes sandbox pod, calling back to the API for tool access
- **Tools**: three kinds — harness built-ins (Read, Bash, etc.), API tools exposed as shell CLI shims, and a headless browser
- **Tool CLIs**: each tool is installed as a shell command at container startup by `services/sandbox/install_tool_shims.py`, which scans `TOOL_DIRS` for `pyproject.toml [project.scripts]` and `uvx`-installs each. Agents call tools directly (`slack get_channel_history '{"channel":"general"}'`, `<tool> --help` to discover) — there is no `call` helper.
- **Slack messaging**: the agent's stdout IS the Slack reply — never call `send_message` on the active thread
- **Rules**: never display secrets, show your work, lead with the answer

`centaur-tools` is the generated catalog CLI emitted by the same installer:
- `centaur-tools list` → list available tool CLIs
- `centaur-tools run <tool> [args]` → run a tool CLI
- `centaur-tools call <tool> <method> [json]` → internal method bridge kept only for the Python workflow host's `ctx.call_tool(...)`; interactive agents use the direct tool CLIs above.

### Persona System

The entrypoint supports persona overlays via `AGENT_PERSONA`. Persona prompts are discovered from the loaded tool directories (including overlays such as `~/centaur-overlay`) and appended after the base + org overlay system prompts at container startup.

### Sandbox Pod Config

- Runs under Kubernetes NetworkPolicies with API reachable through the in-cluster service URL
- Entrypoint injects the runtime URLs and tool catalog environment needed by the sandbox
- Stub API keys so harnesses init in API-key mode (not browser login)
- `HTTPS_PROXY` routes LLM calls through the firewall
- Resource limits: 4GB memory, 2 CPUs
- Image tagged `centaur-agent:latest`
- Labels identify Centaur-managed sandboxes and carry thread/harness metadata for discovery/recovery

### Credential Injection (Firewall)

Sandbox Pods never see real API keys. The firewall (`services/firewall/addon.py`) intercepts HTTPS and injects credentials from the secrets service:

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

- **`sandbox_sessions`** table: tracks sandbox ID, harness, engine, state, thread key, and thread title
- **`chat_messages`** table: stores persisted user/assistant messages for Slackbot delivery and durable transcript surfaces
- On API restart, sandbox ownership is re-read from `sandbox_sessions`; process-local queues and sockets are rebuilt lazily per sandbox
- Pods are still discoverable via Kubernetes labels even if DB state needs reconciliation

## Security Model

- **API auth**: All callers authenticate with DB-backed API keys (`aiv2_*` prefix, stored in `api_keys` table). Local in-cluster service calls use the configured bypass paths where applicable.
- **Sandbox auth**: Sandbox Pods use the runtime's tool and workflow surfaces; agents should not depend on a user-visible Centaur API key.
- **Slack**: HMAC-SHA256 signature verification on all webhooks
- **Public edge**: The Helm chart exposes public routes only when configured through Ingress, HTTPRoute, or service settings.
- **Sandbox isolation**: Pods get stub keys only; real keys injected by firewall proxy in-flight
- **Filesystem**: Host repos mounted read-only by default; only working repo is read-write
- **Kubernetes API**: The API service account is scoped to the Pod, Secret, exec, attach, and log operations needed to manage sandboxes.

## API Key Management

All API authentication uses **DB-backed keys** stored in the `api_keys` Postgres table. Keys are managed via the admin API (localhost-only, or requires `admin` scope).

### Key types

| Type | Prefix | Issued by | Used by | Scopes |
|------|--------|-----------|---------|--------|
| DB keys | `aiv2_*` | Admin API | Slackbot, CLI, external callers | Per-key (e.g. `["*"]`, `["agent:execute"]`) |

### How services get their keys

- **Slackbot**: `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET`, and `SLACKBOT_API_KEY` are injected from the local infra Secret.
- **Sandbox containers**: Use runtime-provided tool CLIs and workflow context rather than a direct Centaur API key
- **Local testing**: Use localhost bypass (no key needed from inside the API deployment), or create a key via admin API

## Secrets

Tool credentials (e.g., `ANTHROPIC_API_KEY`, `AMP_API_KEY`) are never materialized inside sandboxes or the API service. Tools declare which keys they need in their `pyproject.toml` and call `secret("KEY")` to receive a placeholder. Outbound HTTPS traffic is MITM'd by iron-proxy, which substitutes the real credential based on the host/key injection map managed by firewall-manager. iron-proxy resolves `op://...` references directly against 1Password.

For local development, infra secrets are stored in Kubernetes Secrets created by `just bootstrap-secrets`; application secrets continue to come from 1Password.

### iron-control

[iron-control](https://github.com/ironsh/iron-control) is an optional Rails control plane for authenticated API access and encrypted secret storage. It is off by default; enable it with `--set ironControl.enabled=true` (or set `ironControl.enabled: true` in a values file). When enabled, it runs against a dedicated `iron_control_production` database on the bundled Postgres (a separate logical DB so its Rails `schema_migrations` table never collides with the API's dbmate table), created by an idempotent init container.

`just bootstrap-secrets` seeds the required keys into `centaur-infra-env`: the three ActiveRecord encryption keys, `SECRET_KEY_BASE`, and the initial admin password/API key are auto-generated (only when absent, never rotated in place). `IRON_CONTROL_DATABASE_URL` defaults to the bundled Postgres server with no database path (so Rails resolves each connection's database name from the image's `database.yml`); export it before running `just bootstrap-secrets` to point at an external server. Override the admin email with `IRON_CONTROL_INITIAL_USER_EMAIL` (default `admin@centaur.local`).

### centaur-perms

`centaur-perms` is the operator CLI for iron-control permissions: it controls which Slack principals (users and channels) and which roles hold which tool roles and secrets. It lives at `services/api-rs/crates/centaur-perms` and reuses iron-control's canonical mappings (`derive_principal`, `RoleSpec::tool`), so every principal and role `foreign_id` it writes matches exactly what `api-rs` registers at session start. It is the supported way to inspect and edit grants by hand; the API writes the same resources at runtime.

#### Concepts

- **Principal** — a Slack user or channel that an agent session runs as. `foreign_id`s are derived canonically: `slack-channel-<team>-<conv>` for a channel, `slack-user-<team>-<user>` for a DM. A channel's grants win when present; otherwise the session falls back to the requesting user's grants.
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
| `principals grant <principal> [--tool N] [--role F] [--secret OID]` | Grant access. `--tool` registers the tool's `tool-<slug>` role + secrets then assigns it; `--role` assigns an existing role; `--secret` grants a secret OID directly. All repeatable; creates the principal if absent. |
| `principals revoke <principal> [--tool N] [--role F] [--secret OID] [--grant-id OID]` | Reverse of grant. `--tool`/`--role` unassign the role; `--secret` deletes the direct grant for that secret; `--grant-id` deletes a grant by its `grant_…` id. |
| `roles list / show <role>` | List roles, or show the secrets granted to one role. |
| `roles grant <role> [--secret OID] [--tool N [--secret-name NAME]]` | Grant secrets to a role by OID, or register+grant a tool's declared secrets. `--secret-name` (repeatable, requires `--tool`) selects specific declared secrets instead of all. |
| `roles revoke <role> --secret OID` | Revoke one or more secrets from a role (`--secret` required, repeatable). |
| `secrets list [--filter S] [--label k=v] [--managed]` | List secrets across every type, one row per secret. |
| `secrets show <secret>` | Show one secret's full config by OID or `foreign_id` (values are never shown — only the source). |
| `broker create --foreign-id F --token-endpoint URL --client-id ID [--client-secret S] [--refresh-token SEED] [--scope SC]…` | Create or update an iron-control broker credential. Values are passed literally; iron-control owns the OAuth refresh loop. Re-supplying `--refresh-token` re-bootstraps it. |
| `broker list / show <credential> / delete <credential>` | List broker credentials, show one (status/expiry; secret material is never returned), or delete one (by `bcr_` OID or `foreign_id`). |

A `<principal>` argument is treated as a Slack thread key when it contains `:` (e.g. `slack:T123:C456:1700000000.0001`) and run through `derive_principal` — pass `--slack-user` so a DM thread keys to the user. Any value without a `:` is used verbatim as a `foreign_id` (e.g. `slack-channel-t123-c456`) or an OID. Grant/revoke operations are idempotent: re-granting an assigned role or revoking a missing grant is a no-op, reported as such.

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

All services write structured JSON logs to **stdout**. Kubernetes captures pod logs, and optional observability deployments can forward them to VictoriaLogs. VictoriaMetrics receives metrics via push from the API service when enabled.

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
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s "http://victorialogs:9428/select/logsql/query" \
  --data-urlencode "query=thread_key:C042WDDP89Y" --data-urlencode "limit=50"

# API errors in the last hour
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s "http://victorialogs:9428/select/logsql/query" \
  --data-urlencode "query=_stream:{service=\"api\"} AND level:error" --data-urlencode "limit=20"

# Firewall audit trail for a time range
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s "http://victorialogs:9428/select/logsql/query" \
  --data-urlencode "query=_stream:{service=\"firewall\"} AND event:proxy_audit" \
  --data-urlencode "start=2026-03-10T00:00:00Z" --data-urlencode "end=2026-03-11T00:00:00Z"
```

### Audit logging

The **firewall** emits a structured audit event for every outbound request from sandbox containers: method, host, path, status code, request/response bytes, duration, and source container IP. These are searchable via `event:proxy_audit` in VictoriaLogs.

The **API** logs tool calls (`event:tool_call_started`, `event:tool_call_completed`), session lifecycle (`event:warm_container_claimed`), and HTTP requests with thread context.

### Logging contract

Services must write single-line JSON to stdout with these fields:

| Field | Required | Description |
|-------|----------|-------------|
| `timestamp` | Yes | ISO 8601 timestamp |
| `level` | Yes | `debug`, `info`, `warning`, `error` |
| `service` | Yes | Service name (`api`, `firewall`, `secrets`) |
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

### 2. Spawn a runtime assignment

```bash
THREAD_KEY=test-e2e-1

SPAWN=$(kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST http://localhost:8000/agent/spawn \
  -H "Content-Type: application/json" \
  -d "{\"thread_key\":\"${THREAD_KEY}\",\"harness\":\"amp\"}")
ASSIGNMENT_GENERATION=$(printf '%s' "$SPAWN" | jq -r '.assignment_generation')
```

### 3. Persist a message

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST http://localhost:8000/agent/message \
  -H "Content-Type: application/json" \
  -d "{\"thread_key\":\"${THREAD_KEY}\",\"assignment_generation\":${ASSIGNMENT_GENERATION},\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG and nothing else.\"}]}"
```

### 4. Enqueue execution

```bash
EXECUTE=$(kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST http://localhost:8000/agent/execute \
  -H "Content-Type: application/json" \
  -d "{\"thread_key\":\"${THREAD_KEY}\",\"assignment_generation\":${ASSIGNMENT_GENERATION},\"harness\":\"amp\",\"delivery\":{\"platform\":\"dev\"}}")
EXECUTION_ID=$(printf '%s' "$EXECUTE" | jq -r '.execution_id')
```

### 5. Tail durable events (or reconnect later)

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -N \
  "http://localhost:8000/agent/threads/${THREAD_KEY}/events?execution_id=${EXECUTION_ID}&after_event_id=0"
```

If this stream disconnects, reconnect with the last seen `event_id` as `after_event_id`. If the execution already finished, the endpoint emits the terminal `execution_state` snapshot.

### 6. Inspect or cancel

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s "http://localhost:8000/agent/executions/${EXECUTION_ID}" | jq

kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST \
  "http://localhost:8000/agent/executions/${EXECUTION_ID}/cancel" \
  -H "Content-Type: application/json" \
  -d '{}'
```

### 7. Release the assignment when finished

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST "http://localhost:8000/agent/threads/${THREAD_KEY}/release" \
  -H "Content-Type: application/json" \
  -d '{"release_id":"rel-test-e2e-1","cancel_inflight":true}'
```

### Debugging

```bash
kubectl get pods -n centaur -l centaur-agent=true
kubectl exec -n centaur <sandbox-pod> curl -s http://api:8000/health
```
