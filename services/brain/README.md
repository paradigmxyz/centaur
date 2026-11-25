# Brain

`centaur-brain` runs agent turns for the `nanocode` harness outside any
sandbox. Each Centaur thread is a nanocode session with a durable turn inbox
over its own session log, so the brain owns turn queueing, steering,
cancellation, crash recovery, and the transcript. api-rs stays the front door:
it authorizes the caller, then submits turns, interrupts, and proxies the
event stream. It writes no execution rows or session events for these turns.
The service is off by default (`brain.enabled`).

## HTTP

The service listens on `0.0.0.0:8790`:

```bash
centaur-brain serve --listen 0.0.0.0:8790
```

Thread keys are percent-encoded path segments.

| Route | Purpose |
|-------|---------|
| `GET /healthz` | Readiness and liveness. |
| `POST /v1/threads/{thread_key}/turns` | Submit a turn: `{"request_id", "input_line", "steer", "tool_token"}`. Idempotent by `request_id`; a different input under the same id is a 409. `steer: true` folds the turn into the running one when there is one. `tool_token` is optional; see [Code Mode](#code-mode). |
| `GET /v1/threads/{thread_key}` | The thread's unfinished turns: `{"running", "queued"}`. |
| `POST /v1/threads/{thread_key}/interrupt` | Cancel the running turn. |
| `GET /v1/threads/{thread_key}/events?after_event_id=N[&execution_id=R]` | The thread's session event stream, in the same SSE format as api-rs `/api/session/{key}/events`. |

`input_line` is one blocks-protocol user line, as api-rs would write it to a
harness. The request id is the execution id clients see. A thread gets a fresh
UUIDv7 nanocode session id the first time it appears, recorded in
`nanocode.thread_sessions` (in memory without a database).

## Event stream

Events are projected from the session log on every read, so any cursor can be
replayed and the stream survives restarts. Each turn yields
`session.execution_started`, `session.output.line` events carrying Codex
app-server notifications (translated from nanocode events by harness-server's
turn normalizer), `session.first_token`, and exactly one of
`session.execution_completed` (with `result_text`), `session.execution_failed`,
or `session.execution_cancelled`. Event ids are `seq * 64 + k` for log seq
`seq`: they increase monotonically per thread and are stable across replay.
With `execution_id`, the stream carries only that turn and ends after its
terminal event.

The brain commits the log before it publishes events, so a client never sees an
event that a crash could take back. After a crash, nanocode resumes an
interrupted turn from its last commit point, up to three times.

## Code Mode

With Code Mode configured (`CENTAUR_CODEMODE_EXECUTOR_URL` and
`CENTAUR_CODEMODE_TOKEN_PUBLIC_KEY`), every thread gets three native nanocode
tools, defined by `centaur_codemode_protocol::tools`:
`centaur_catalog_search` and `centaur_catalog_load` call the executor's catalog
(`/v1/search`, `/v1/load`), and `centaur_execute` runs a cell on it (`/v1/exec`,
with the model's tool call id as `cell_id`). The Code Mode instructions join the
session's developer instructions. The startup log says whether the tools are on.

api-rs sends a turn token as `tool_token` with each turn it submits. The brain
verifies it (signature, issuer, audience, expiry) and that its `thread_key` is
the thread's. A token that fails, or any token when no public key is
configured, is logged and dropped: the turn still runs, only without Code
Mode, so a key mismatch never fails every turn.

The token lives in its turn's own nanocode metadata, and nowhere else: nanocode
stores it with the turn, keeps it across a restart for a resumed turn, and
retires it with the turn. A tool call reads the thread's running turn and uses
the token in that turn's record, so a turn queued behind it, with another
requester's token, never changes the running turn's access, and a turn
submitted without a token has none. A retry of the same request keeps the
token it was first submitted with. A steer mints no token and folds into the
running turn, so it uses that turn's.

Each tool call sends the turn's token as `Authorization: Bearer` and
nothing else, directly rather than through any egress proxy. The executor
verifies it, runs the cell or catalog call in-process, and presents the same
token to the tenant's iron-proxy for connector upstream credentials, so the
brain never holds a credential of its own. A turn that never received a
token, or whose token has expired, gets an error result instead of a call.
Search and load time out after 20 s, cells after 12 minutes, and answers are
capped at 24 MiB. A `503` from the executor tells the model to retry; a `401`
means the turn's access was refused or has expired, and reads as such.

Search and load have no effect class, so a resumed turn simply repeats them.
`centaur_execute` is an external effect: its call is committed to the session
log before the cell runs, and a turn resumed after a crash mid-cell records
the cell's outcome as unknown instead of running it again. Tool calls appear on
the event stream as Codex `mcpToolCall` items (server `centaur`) with their
arguments and result text or error.

## Migrations

The brain owns its schema, separately from api-rs. `centaur-brain migrate`
applies `crates/centaur-brain/migrations` (the `nanocode` schema and the
`centaur_brain` role, created NOLOGIN) using `DATABASE_URL`, a role that may
create schemas and roles. It records them in its own
`centaur_brain_migrations._sqlx_migrations` table, apart from api-rs's tracking
table and out of the brain role's reach. Given `BRAIN_DATABASE_URL` too, it then
sets the role's login password from that DSN, which must connect as
`centaur_brain`. The chart runs it as an init container of the brain Deployment
when `brain.databaseUrl` is set; only that init container receives the owner
DSN. nanocode creates its own tables inside `nanocode` at runtime.

## Environment

- `BRAIN_DATABASE_URL` — optional Postgres DSN for nanocode durability,
  connecting as the `centaur_brain` role, whose default search_path is the
  `nanocode` schema. When unset the service keeps state in memory, which is for
  local development only. Store the DSN, with a password you choose, in a
  Secret named by `brain.databaseUrl.existingSecretName`; the migration init
  container enables the login with that password on every start, so rotating
  it means changing the Secret and restarting the pod.
- `CENTAUR_CODEMODE_TOKEN_PUBLIC_KEY` — SPKI PEM Ed25519 key that verifies
  turn tokens. Without it, submitted tokens are ignored and the Code Mode tools
  stay off.
- `CENTAUR_CODEMODE_EXECUTOR_URL` — base URL of the Code Mode executor. With the
  public key, this turns on the Code Mode tools.
- `BRAIN_DEFAULT_MODEL` — default model for turns that name none; defaults to
  `luna`.
- `BRAIN_DEFAULT_THINKING` — default reasoning effort; defaults to `low`.
- `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` — model provider credentials. The chart
  sets the placeholders `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` (the same
  literal strings) unless `brain.extraEnv` overrides them: behind a proxy those
  placeholders are what iron-proxy substitutes on the wire, and a direct key can
  be supplied through `extraEnv` instead.
- `OPENAI_BASE_URL` — optional API base override, from `brain.openaiBaseUrl`.
  Empty uses the provider default, `https://api.openai.com/v1`, and nanocode
  calls its Responses API over HTTPS with SSE (`{base}/responses`,
  `Accept: text/event-stream`).
- `HTTPS_PROXY`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NO_PROXY` — honored
  process-wide, so the service can reach model providers through an egress
  proxy. nanocode does not merge these over the platform store: when
  `SSL_CERT_FILE` or `SSL_CERT_DIR` is set it loads trust roots only from those
  paths. `brain.trustReleaseCa` therefore keeps `SSL_CERT_FILE` on the image's
  public bundle (`/etc/ssl/certs/ca-certificates.crt`, 148 roots) and adds the
  release CA through `SSL_CERT_DIR=/etc/centaur-ca`; nanocode reads every regular
  PEM file in that directory and unions the two sets. A single concatenated file
  built by an init container would also work, but this needs no writable volume,
  no extra container, and no file assembly.

Model traffic goes directly over HTTPS. Production model egress should go
through a dedicated iron-proxy with a model-only principal; that is a
follow-up, not part of this deployment wiring.

## Chart

The chart renders a Deployment, Service, and NetworkPolicies when
`brain.enabled=true` and points api-rs at the Service with
`CENTAUR_BRAIN_URL`.

`brain.trustReleaseCa=true` mounts `firewall.existingCaSecretName` (the public
`ca-cert.pem` entry; the private key lives in a separate Secret and is never
projected) at `/etc/centaur-ca` and sets the two trust variables above. This is
how the e2e stack makes provider hostnames — which CoreDNS rewrites to its fake
model server — verify: that server's certificate is signed by the release CA.
