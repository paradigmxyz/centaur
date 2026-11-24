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
| `POST /v1/threads/{thread_key}/turns` | Submit a turn: `{"request_id", "input_line", "steer"}`. Idempotent by `request_id`; a different input under the same id is a 409. `steer: true` folds the turn into the running one when there is one. |
| `GET /v1/threads/{thread_key}` | The thread's unfinished turns: `{"running", "queued"}`. |
| `POST /v1/threads/{thread_key}/interrupt` | Cancel the running turn. |
| `GET /v1/threads/{thread_key}/events?after_event_id=N[&execution_id=R]` | The thread's session event stream, in the same SSE format as api-rs `/api/session/{key}/events`. |

`input_line` is one blocks-protocol user line, as api-rs would write it to a
harness. The request id is the execution id clients see.

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

## Environment

- `BRAIN_DATABASE_URL` — optional Postgres DSN for nanocode durability. The
  role's default search_path is the `nanocode` schema (see the
  `centaur_brain` role migration in `crates/centaur-session-sqlx/migrations`).
  When unset the service keeps state in memory, which is for local development
  only. Provision a login for the role out of band
  (`ALTER ROLE centaur_brain LOGIN PASSWORD ...`) and store the DSN in a Secret;
  the Helm value is `brain.databaseUrl.existingSecretName`.
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
