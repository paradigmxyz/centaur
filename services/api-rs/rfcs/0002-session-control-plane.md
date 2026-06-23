# RFC 0002: Session Control Plane

Status: Draft
Owner: TBD
Target: `services/api-rs`

## Summary

Introduce `Session` as the durable control-plane object above the sandbox
runtime abstraction from RFC 0001. A session represents one ongoing agent
conversation, regardless of whether it was started by a chat app, a workflow, a
CLI, or another caller.

The session is the source of truth for:

- the external thread identity
- the current sandbox assignment
- the harness type
- the harness-native thread/session identifier
- persisted input messages
- execution state
- replayable output events

The public API should be small:

- `POST /api/session/{thread_key}`: idempotently create or return a session
- `POST /api/session/{thread_key}/messages`: append durable input
- `POST /api/session/{thread_key}/execute`: run the session
- `GET /api/session/{thread_key}/events`: stream or replay session output over
  SSE

## Goals

- Make `Session` the core control-plane model.
- Use `thread_key` as the public identity and database primary key for a
  session.
- Keep exactly one current `sandbox_id` on a session. If a sandbox dies and is
  replaced, the session row points at the new sandbox.
- Persist `harness_type` and `harness_thread_id` on the session so the control
  plane can resume the correct harness conversation.
- Keep clients thin: create/get session, append messages, execute, stream events.
- Make output replayable through durable session events, not process-local
  streams.
- Keep this layer independent from any specific caller such as a chat app or
  workflows.

## Non-goals

- Exposing sandbox create, pause, resume, or stop as public API endpoints.
- Designing every migration detail from the current Python API.
- Preserving `assignment_generation` as a public client concept.
- Designing chat final delivery, workflow scheduling, tool discovery, API key
  management, or persona selection in detail.
- Making `sandbox_id` historical. The session stores the current sandbox only;
  historical sandbox lifecycle can be represented as events if needed.

## Relationship to RFC 0001

RFC 0001 defines a backend-neutral sandbox runtime layer. This RFC defines the
durable Centaur control-plane object that calls that layer.

The split is:

- sandbox layer: creates isolated workloads and moves bytes
- session layer: decides which sandbox belongs to which conversation, persists
  caller-supplied input/output bytes as lines, and exposes replayable state to
  clients

`Session` should not leak backend transport details. It may store a
`sandbox_id`, but clients should not attach to that sandbox directly.

## Core Model

A session is one durable agent conversation.

```text
Session {
  thread_key: string        // primary key
  sandbox_id: string | null
  harness_type: string
  harness_thread_id: string | null
  status: active | executing | idle | failed | archived
  created_at: timestamp
  updated_at: timestamp
}
```

Field notes:

- `thread_key` is the unique public key and storage primary key. For chat apps
  it can be the chat thread key. For workflows it can be a workflow-owned key.
  For CLI or test callers it can be any caller-owned stable key.
- `sandbox_id` is the current runtime assignment. It is nullable before the
  first execution and can be overwritten when the control plane replaces a dead
  sandbox.
- `harness_type` selects the harness adapter, for example `amp`,
  `claude-code`, or `codex`.
- `harness_thread_id` is the harness-native conversation identifier when the
  harness exposes one. It is nullable until the first harness turn returns it.

### Ownership

The session row is authoritative. In-memory workers, live SSE connections, and
sandbox observations are caches or transports. On restart, the API should be
able to recover session state from the database and continue from the latest
durable message, execution, event, and current sandbox binding.

### Sandbox Replacement

A session has at most one current sandbox. If the current sandbox is gone or
unhealthy, the control plane may create a replacement sandbox and atomically
overwrite `session.sandbox_id`.

Replacement should emit lifecycle events so operators can see what happened, but
clients should continue addressing the session, not the sandbox.

## Durable State

The first implementation can use a small set of tables:

| Table | Purpose |
|-------|---------|
| `sessions` | One row per durable conversation, keyed by `thread_key` |
| `session_messages` | Durable user, assistant, system, and tool-visible input/output messages keyed by `thread_key` |
| `session_executions` | One row per requested execution keyed by `thread_key` |
| `session_events` | Replayable projected output and lifecycle events keyed by `thread_key` |

The exact schema can evolve, but the invariant should hold: if a client has a
`thread_key` and last seen event id, it can reconnect and recover session output
without relying on a still-open process stream.

`thread_key` should be constrained rather than arbitrary unbounded text:

- namespaced by source, for example `chat:C123:1780000000.000000`
- stable for the lifetime of the conversation
- globally unique, or unique inside an explicit tenant scope
- bounded to a practical maximum length, for example 512 bytes
- not raw JSON, raw URLs, or other unbounded caller payloads

## API

### Create or Get Session

```text
POST /api/session/{thread_key}
```

Creates a session for `thread_key` or returns the existing session. The
`thread_key` path segment must be URL-encoded by clients when it contains
reserved characters.

Example request:

```json
{
  "harness_type": "amp",
  "metadata": {
    "source": "chat",
    "channel_id": "C123",
    "user_id": "U123"
  }
}
```

Example response:

```json
{
  "thread_key": "chat:C123:1780000000.000000",
  "sandbox_id": null,
  "harness_type": "amp",
  "harness_thread_id": null,
  "status": "idle"
}
```

Idempotency rules:

- `thread_key` is unique.
- Repeating the same request returns the existing session.
- If the existing session has a different `harness_type`, the API should reject
  the request with `409 Conflict` rather than silently changing the session.

### Append Messages

```text
POST /api/session/{thread_key}/messages
```

Appends one or more durable messages to the session. This endpoint persists
input; it does not execute the harness by itself.

Example request:

```json
{
  "messages": [
    {
      "role": "user",
      "parts": [
        {"type": "text", "text": "Reply with exactly PONG and nothing else."}
      ],
      "metadata": {
        "source": "chat",
        "user_id": "U123"
      }
    }
  ]
}
```

Example response:

```json
{
  "ok": true,
  "message_ids": ["msg_123"]
}
```

Message `parts` are durable transcript data for clients and operators. The API
stores them but does not translate them into harness-specific stdin during
execution; the caller that invokes `/execute` supplies the opaque input lines
that should be written to the sandbox.

### Execute Session

```text
POST /api/session/{thread_key}/execute
```

Requests one execution of the session. The API serializes executions per
session, ensures there is a usable current sandbox, writes caller-supplied input
lines to sandbox stdin, and stores each stdout line as a durable session event.

The API is deliberately oblivious to the producer/consumer format. Codex
app-server JSONL, Anthropic-shaped turns, or any future protocol are just opaque
newline-delimited strings at this layer.

Example request:

```json
{
  "input_lines": [
    "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG and nothing else.\"}]}}"
  ],
  "idle_timeout_ms": 1000,
  "max_duration_ms": 60000,
  "metadata": {
    "source": "chat"
  }
}
```

Example response:

```json
{
  "ok": true,
  "execution_id": "exe_123",
  "thread_key": "chat:C123:1780000000.000000",
  "status": "queued"
}
```

Execution rules:

- Only one execution may run for a session at a time.
- If no sandbox is assigned, the API creates one and stores its `sandbox_id`.
- If the assigned sandbox is gone, the API replaces it and overwrites
  `sandbox_id`.
- `input_lines` are written to sandbox stdin exactly as newline-delimited lines.
- Each stdout line is appended to `session_events` before being streamed to
  clients as `session.output.line`.
- Execution completion is transport-level. The first implementation may use an
  idle timeout and max duration; it must not parse Codex, harness, or app-server
  event names to decide completion.
- If a producer or consumer discovers a harness-native thread id, it can report
  that separately; the session API should not infer it by parsing output lines.

### Stream Session Events

```text
GET /api/session/{thread_key}/events?after_event_id=0
```

Streams durable session events as Server-Sent Events. The stream is scoped to the
session, not to a live worker. A client can disconnect and reconnect with the
last seen event id.

Example stream:

```text
id: 101
event: session.execution_started
data: {"execution_id":"exe_123","thread_key":"chat:C123:1780000000.000000"}

id: 102
event: session.output.line
data: {"type":"item.agentMessage.delta","delta":"P"}

id: 103
event: session.output.line
data: {"type":"item.agentMessage.delta","delta":"ONG"}

id: 104
event: session.execution_completed
data: {"execution_id":"exe_123","output_line_count":2}
```

SSE rules:

- Every event has a monotonically increasing session event id.
- `after_event_id` is exclusive.
- If there are existing events after `after_event_id`, the API replays them
  before waiting for new events.
- `session.output.line` event `data:` is the raw line, not JSON encoded again by
  the API. It may contain Codex app-server JSONL, another JSONL protocol, or
  plain text.
- Terminal execution state is represented as a durable event.
- The session stream does not close just because one execution reaches a
  terminal state; callers may keep one SSE connection open across later
  executions on the same session.
- Clients do not need to know the current `sandbox_id` to stream output.

## Control-Plane Flow

```text
caller
  POST /api/session/{thread_key}
    -> create or get session

caller
  POST /api/session/{thread_key}/messages
    -> persist user input

caller
  POST /api/session/{thread_key}/execute
    -> get execution_id

caller
  GET /api/session/{thread_key}/events?after_event_id=...
    -> replay and stream output
```

Internally, execution follows this shape:

```text
load session
claim session execution lock
ensure current sandbox exists
write request input_lines to sandbox stdin
persist stdout lines as session.output.line events
mark execution terminal
release execution lock
```

## Compatibility Path

The current Python API has separate `spawn`, `message`, `execute`, and thread
events endpoints. The session API keeps the same useful client shape but removes
assignment generation from the protocol and keeps `thread_key` as the only
session address.

A migration can be incremental:

1. Add session tables and endpoints.
2. Implement the endpoints as wrappers around the existing execution machinery.
3. Teach chat adapters, workflows, and CLI callers to use sessions.
4. Keep legacy endpoints as compatibility shims until callers have moved.
5. Remove legacy public concepts once sessions are the only client contract.

## Testing Strategy

- Unit test idempotent session creation by `thread_key`.
- Unit test `thread_key` as the canonical public address for messages,
  execution, and events.
- Unit test `409 Conflict` on incompatible `harness_type`.
- Unit test message append ordering.
- Unit test per-session execution serialization.
- Unit test sandbox replacement updates only the current `sandbox_id`.
- Unit test `/execute` writes opaque `input_lines` without inspecting them.
- Unit test stdout lines are stored and replayed without parsing or JSON
  re-encoding.
- Integration test create -> messages -> execute -> events replay.
- Integration test SSE reconnect with `after_event_id`.
- Integration test API restart recovery from persisted session state.

## Open Questions

- Should `harness_type` be immutable for the lifetime of a session?
- How much transcript history should each execution send to the harness by
  default?
- Should cancellation be part of this RFC, for example
  `POST /api/session/{thread_key}/cancel`?
- Should archival release the current sandbox immediately or keep it warm for a
  short grace period?

## Recommendation

Start with `Session` as the only public durable conversation object. Keep the
API small, make `thread_key` the canonical public address and primary key, treat
`sandbox_id` as a replaceable current binding, and make SSE replay come from
durable session events. This gives chat adapters, workflows, and future clients
one protocol without exposing sandbox lifecycle details.
