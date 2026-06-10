# RFC 0003: Absurd-Backed Python Workflow Host

Status: Draft
Owner: TBD
Target: `services/api-rs`

## Summary

Move workflow orchestration into `services/api-rs` using Absurd as the durable
Postgres-backed task engine, while preserving the existing Python workflow
authoring model.

The Rust API should own workflow runs, leases, retries, cancellation,
checkpoint state, webhook ingress, and schedule firing. Existing workflow files
under `workflows/` and deployment overlays should continue to run through a
dedicated Python workflow host that imports `WORKFLOW_NAME`, optional `Input`,
`WEBHOOKS`, `SCHEDULE`, and `handler(params, ctx)`.

The Python host is a workflow harness. It is not `centaur_sdk/`. It is a small
sandboxed runner that executes workflow Python and calls back to api-rs for
durable context operations such as `ctx.step`, `ctx.agent_turn`, tool calls,
Slack posting, and logging. For the first compatibility version, workflows get
direct access to the main Postgres database through `ctx._pool`.

## Goals

- Make Absurd the durable control plane for workflow execution and schedules.
- Preserve the existing Python workflow file contract.
- Run workflows in their own sandbox/workload class, separate from agent
  sandboxes.
- Expose a compatibility `WorkflowContext` for existing workflows.
- Support webhook-triggered workflows without routing through the Python API.
- Host schedules as Absurd tasks so schedule firing is restart-safe and
  idempotent.
- Give workflows direct main database access for the compatibility phase.
- Isolate long-running ETL schedules from user-facing and webhook workflows.

## Non-goals

- Rewriting the existing Python workflow catalog in Rust.
- Making `slack_thread_turn` a generic Python-hosted workflow. Slack thread turn
  handling should become native api-rs session adapter logic.
- Designing final least-privilege database roles for workflow sandboxes.
- Replacing the complete tool plugin runtime in the first implementation.
- Removing the Python API immediately.
- Implementing every Cloudflare Workflows-like primitive in the first PR.

## Existing Behavior to Preserve

The Python workflow engine discovers workflow modules from `WORKFLOW_DIRS`.
Each workflow file may export:

- `WORKFLOW_NAME`
- optional dataclass `Input`
- optional `WEBHOOKS`
- optional `SCHEDULE`
- async `handler(inp, ctx)`

Existing handlers assume a `WorkflowContext` with some combination of:

- `ctx.run_id`
- `ctx.step(name, fn)`
- `ctx.agent_turn(...)`
- `ctx.call_tool(tool, method, args)`
- `ctx.post_to_slack(channel, text, ...)`
- `ctx.log(message, **fields)`
- `ctx._pool`

Several important workflows issue custom SQL through `ctx._pool`. For the
compatibility version, the Python host should provide a real `asyncpg` pool to
the same database api-rs uses.

## Architecture

```text
client / webhook / schedule
        |
        v
api-rs workflow routes
        |
        v
Absurd queue: centaur_workflows
        |
        +--> native Rust task handlers
        |
        +--> Python workflow task
                 |
                 v
           workflow-python sandbox
                 |
                 v
           workflow_host.py imports workflow module
                 |
                 v
           handler(input, WorkflowContext)
                 |
                 +--> ctx.step       -> api-rs / Absurd checkpoint RPC
                 +--> ctx.agent_turn -> api-rs SessionRuntime
                 +--> ctx.call_tool  -> api-rs tool route
                 +--> ctx.post_to_slack
                 +--> ctx._pool      -> direct Postgres
```

### Rust Runtime

`centaur-workflows` owns:

- workflow run creation
- Absurd queue setup
- task registration
- checkpoint read/write helpers
- webhook registry metadata
- schedule tick tasks
- dispatch to native Rust handlers or the Python host

`centaur-api-server` owns:

- HTTP routes
- webhook request parsing and auth
- public workflow run APIs
- RPC endpoints or stdio bridge messages consumed by the Python host

### Python Workflow Host

Add a workflow host script, for example:

```text
services/workflow-python/workflow_host.py
```

or inside the sandbox image if we reuse that image family:

```text
services/sandbox/workflow_host.py
```

The host:

- reads `WORKFLOW_DIRS`
- imports workflow modules
- builds a registry keyed by `WORKFLOW_NAME`
- exposes discovery for `WEBHOOKS` and `SCHEDULE`
- hydrates `Input` dataclasses when present
- runs `await handler(inp, ctx)`
- speaks NDJSON over stdin/stdout to api-rs

The host should not implement durable workflow state itself. Durability lives in
Absurd and api-rs.

## Host Protocol

Use stdio NDJSON for the first implementation. This matches the existing
sandbox byte-I/O abstraction and avoids adding another network service inside
the workflow sandbox.

### Start Workflow

api-rs sends:

```json
{
  "type": "workflow.start",
  "run_id": "019...",
  "task_id": "019...",
  "workflow_name": "github_issue_triage",
  "input": {
    "webhook": {
      "slug": "github-issue-triage",
      "headers": {},
      "query": {},
      "body": {}
    }
  }
}
```

The host returns:

```json
{
  "type": "workflow.result",
  "result": {}
}
```

or:

```json
{
  "type": "workflow.error",
  "message": "..."
}
```

### Context RPC

While a workflow is running, the host may send requests:

```json
{"type":"ctx.step.get","request_id":"1","step":"load_state"}
{"type":"ctx.step.put","request_id":"2","step":"load_state","value":{}}
{"type":"ctx.agent_turn","request_id":"3","args":{}}
{"type":"ctx.call_tool","request_id":"4","tool":"slack","method":"send_message","args":{}}
{"type":"ctx.post_to_slack","request_id":"5","channel":"C123","text":"hello","args":{}}
{"type":"ctx.log","request_id":"6","message":"workflow_event","fields":{}}
```

api-rs responds:

```json
{"type":"ctx.response","request_id":"1","ok":true,"value":null}
```

or:

```json
{"type":"ctx.response","request_id":"1","ok":false,"error":"..."}
```

## WorkflowContext Compatibility

The Python host provides a compatibility context:

```python
class WorkflowContext:
    run_id: str
    task_id: str
    _pool: asyncpg.Pool

    async def step(self, name, fn, *, retry=None, timeout=None): ...
    async def agent_turn(self, text=None, **kwargs): ...
    async def call_tool(self, tool, method, args=None): ...
    async def post_to_slack(self, channel, text, **kwargs): ...
    def log(self, message, **fields): ...
```

### `ctx.step`

`ctx.step(name, fn)` must preserve checkpoint/replay semantics:

1. Ask api-rs for the Absurd checkpoint for `(task_id, step_name)`.
2. If present, return the cached value without calling `fn`.
3. If absent, run `fn`.
4. Serialize the result as JSON.
5. Store it through api-rs / Absurd.
6. Return the result.

First version assumes step results are JSON-serializable. Non-serializable
values should fail clearly.

### `ctx._pool`

For the compatibility phase:

- inject `DATABASE_URL` into workflow sandboxes
- create `asyncpg.create_pool(DATABASE_URL)`
- expose it as `ctx._pool`
- allow network access from workflow sandboxes to Postgres
- use the same DB role as api-rs initially

This is deliberate POC debt. The follow-up hardening path is a dedicated
workflow DB role, table grants, and read-only pools for selected workflows.

### `ctx.agent_turn`

api-rs handles agent turns through `SessionRuntime`.

Rules:

- honor explicit `thread_key`
- otherwise derive `wf:<task_id>:agent:<step-or-message>`
- honor explicit `message_id`
- otherwise derive a deterministic message id from task id and call site
- pass through `metadata`, `delivery`, `harness`, `persona`, and prompt override
- wait for terminal session result and return the same result shape existing
  workflows expect

### `ctx.call_tool`

api-rs should call the tool runtime and return JSON output. If api-rs tool
runtime is incomplete during the POC, this can temporarily proxy to the existing
Python API, but the target design is api-rs-owned tool routing.

### `ctx.post_to_slack`

`ctx.post_to_slack` should be implemented through the Slack tool or a small
api-rs Slack helper. Slack side effects must include deterministic
`client_msg_id`, derived from `(task_id, step_name/call_index)`, unless the
workflow supplies one.

## Webhooks

api-rs owns webhook ingress at:

```text
POST /api/webhooks/{slug}
```

The route should:

- look up the registered slug
- enforce allowed methods and content types
- verify HMAC, GitHub HMAC, or bearer auth
- redact sensitive headers
- parse JSON or form payloads
- preserve the Python-compatible input envelope
- spawn an Absurd workflow task with deterministic idempotency

Input envelope:

```json
{
  "webhook": {
    "slug": "github-issue-triage",
    "provider": "github",
    "method": "POST",
    "path": "/api/webhooks/github-issue-triage",
    "headers": {},
    "query": {},
    "body": {},
    "raw_body_sha256": "..."
  }
}
```

### Webhook Discovery

The first POC may keep a static registry plus `WORKFLOW_WEBHOOKS_JSON`. The
production path should reconcile webhook specs from the Python host:

1. Start workflow host.
2. Ask it to discover workflows.
3. Receive `workflow_name`, source path, `WEBHOOKS`, and `SCHEDULE`.
4. Register slug-to-workflow mappings in api-rs.

## Absurd-Hosted Schedules

Schedules should be hosted in Absurd, not an in-memory api-rs ticker.

Each enabled schedule definition is reconciled into a durable Absurd task:

```text
centaur.workflow.schedule_tick(schedule_id)
```

Schedule tick flow:

1. Load the discovered schedule definition.
2. Sleep until `next_run_at`.
3. On wake, compute a deterministic fire key:

   ```text
   schedule:<schedule_id>:<scheduled_at>
   ```

4. Spawn the target workflow with `idempotency_key = fire_key`.
5. Compute the next run time.
6. Spawn or schedule the next `schedule_tick`.

Ticks should be finite tasks. Avoid one endless task loop unless Absurd's
long-lived sleep semantics prove operationally better. A finite tick is easier
to inspect, retry, and cancel.

### Schedule Definition Source

Initial source of truth:

- Python workflow files export `SCHEDULE`.
- api-rs reconciles discovered schedules at startup and on workflow reload.

Later:

- persist discovered schedule metadata for admin UI and introspection
- track enabled/disabled state and last fire state separately from source files

### Schedule Types

Support:

- interval schedules
- cron schedules
- timezone field for cron schedules
- `no_delivery` metadata for workflows that should not final-post to a chat

## Queue Isolation

Workflow classes should not share one unconstrained worker pool.

Suggested classes:

| Class | Examples | Concurrency |
|-------|----------|-------------|
| `agent` | `agent_turn`, prompt/report workflows | medium/high |
| `webhook` | GitHub/Trivy intake workflows | medium/high |
| `etl` | `slack_sync`, `slack_backfill`, `company_context_documents` | low |
| `maintenance` | schema/bootstrap/reporting jobs | low |

Known heavy workflows should default to `etl`:

- `slack_sync`
- `slack_backfill`
- `company_context_documents`
- parts of `chief_of_staff_daily`

This avoids repeating the Python API failure mode where long ETL workflows
occupy worker slots and delay Slack thread turns.

## Compatibility Matrix

### Works With Native Rust or Minimal Host

- `echo`
- simple `agent_turn` tests

### Agent/Webhook Workflows

Expected to work once Python host, webhook envelope, and `ctx.agent_turn` exist:

- `github_issue_triage`
- `consensus_ci_triage`
- `trivy_vulnerability_intake`
- `seo_analysis`
- `supply_chain_security_report`
- `deel_system_review`
- `paradigm_pulse_daily`

### DB-Backed Business Workflows

Expected to work once `ctx.step` and `ctx._pool` exist:

- `muesli_meeting_ingest`
- `monitorink`
- `chief_of_staff_daily`
- `tempo_events_linear_projects`

### ETL/Scheduled Workflows

Expected to work after schedule reconciliation, DB access, env propagation,
Slack client imports, and queue isolation:

- `slack_sync`
- `slack_backfill`
- `company_context_documents`

### Native api-rs Slack Adapter

`slack_thread_turn` should not be hosted as generic Python workflow code. It
should become native api-rs Slack/session adapter logic:

- hydrate Slack history
- parse harness/persona prompt switches
- release/reassign sessions on prompt switch
- call native session runtime
- preserve final delivery/outbox behavior

## Implementation Plan

### Phase 1: Python Host Skeleton

- Add workflow host script.
- Support workflow discovery.
- Support `workflow.start` and `workflow.result`.
- Hydrate dataclass `Input`.
- Run simple Python `echo` workflow in local sandbox.

### Phase 2: Context RPC

- Implement `ctx.log`.
- Implement `ctx.agent_turn` through `SessionRuntime`.
- Implement `ctx.call_tool`.
- Implement `ctx.post_to_slack` with deterministic `client_msg_id`.
- Validate agent/webhook workflows.

### Phase 3: Checkpoints and DB

- Implement `ctx.step` backed by Absurd checkpoints.
- Inject `DATABASE_URL`.
- Implement `ctx._pool` as `asyncpg` pool.
- Validate `muesli_meeting_ingest`, `monitorink`, and `chief_of_staff_daily`.

### Phase 4: Discovery Reconciliation

- Reconcile `WEBHOOKS` from Python host into api-rs registry.
- Reconcile `SCHEDULE` from Python host into Absurd schedule tick tasks.
- Keep static registry and `WORKFLOW_WEBHOOKS_JSON` only as debugging fallback.

### Phase 5: Schedules and Queue Classes

- Add Absurd schedule tick task.
- Add cron and interval next-run calculation.
- Add queue/class routing.
- Validate ETL workflows without starving user-facing workflows.

### Phase 6: Production Hardening

- Dedicated workflow DB role.
- Network policy specifically for workflow sandboxes.
- Workflow dependency packaging.
- Admin/introspection surfaces for schedules and runs.
- Replay tests for duplicate Slack/tool prevention.

## Testing Plan

- Unit test workflow discovery and duplicate workflow names.
- Unit test dataclass input hydration.
- Unit test webhook auth, redaction, and idempotency.
- Unit test `ctx.step` checkpoint replay.
- Integration test direct `ctx._pool` access against local Postgres.
- Local sandbox smoke for Python host.
- Kind smoke for workflow-python sandbox.
- End-to-end webhook workflow:
  - receive webhook
  - enqueue Absurd task
  - run Python handler
  - call `ctx.agent_turn`
  - return completed result
- Replay test:
  - run workflow once
  - replay/retry
  - verify checkpointed step does not duplicate Slack/tool side effects

## Open Questions

- Should workflow host reuse the existing sandbox image or get a dedicated
  smaller image?
- How should Python workflow dependencies be packaged for overlays?
- Should schedule metadata be persisted immediately, or only reconciled from
  source at startup for the first PR?
- What is the minimum api-rs tool runtime needed before `ctx.call_tool` can stop
  proxying to the Python API?
- Which workflows require file attachment upload/download support in the first
  compatibility release?

## Acceptance Criteria

- api-rs can run an existing Python workflow file through Absurd.
- `ctx.agent_turn`, `ctx.step`, `ctx._pool`, and Slack/tool side effects work
  for at least one real workflow.
- Webhook delivery to a Python workflow is idempotent.
- An Absurd-hosted schedule fires a workflow and reschedules its next tick.
- ETL schedule execution does not block webhook or agent workflow execution.
