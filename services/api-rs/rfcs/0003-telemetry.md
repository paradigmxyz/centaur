# RFC 0003: API-RS Telemetry

Status: Draft
Owner: TBD
Target: `services/api-rs`

## Summary

Add a shared telemetry layer for the Rust API control plane that standardizes
logs, metrics, and traces without leaking HTTP or OpenTelemetry concepts into
the sandbox abstraction.

The first implementation slice introduces a `centaur-telemetry` crate. The API
server uses that crate for JSON `tracing` logs by default and optional OTLP trace
export when an OTLP endpoint is configured. Later slices add HTTP middleware,
Prometheus/VictoriaMetrics metrics, and domain spans in the session runtime.

## Implementation Status

- Slice 1 is implemented: `centaur-telemetry` initializes JSON logs, optional
  OTLP tracing, service metadata, shared field constants, and shutdown flushing.
- Slice 2 is implemented: `centaur-api-server` uses `tower-http` for HTTP
  request spans, `metrics` plus `metrics-exporter-prometheus` for route-template
  HTTP metrics, structured request logs, and `/metrics`.
- Slice 3 is implemented: `centaur-session-runtime` and
  `centaur-sandbox-manager` emit domain spans and structured logs for session
  create, message append, execute, sandbox ensure/create/open/stop, input
  writes, stdout pumping, and event-stream setup.
- Slice 4 is implemented: `centaur-session-runtime` and
  `centaur-sandbox-manager` emit bounded Prometheus domain metrics for session
  execution lifecycle, execution duration, sandbox backend operations, and
  warm-pool claim results.
- Metrics scrape readiness is implemented: the Helm chart emits Prometheus
  scrape annotations for `api-rs` by default. Dashboard and Grafana provisioning
  artifacts are intentionally not checked in with this slice.
- Codex app-server event spans are implemented: parsed sandbox stdout JSON
  emits `centaur.api_rs.codex_app_server.event` spans, and recognized tool-call
  envelopes emit `centaur.api_rs.codex_app_server.tool_call` spans with bounded
  tool identity and status attributes only.
- Process-local trace continuity is implemented for spawned stdout work:
  `centaur.api_rs.session.execution` is created when an execution is claimed,
  kept active until terminal state, and used as the parent for stdout-pump,
  codex app-server event, and tool-call spans emitted by the background pump.
- Harness trace export wiring is implemented: every sandbox stdin line carries
  `thread_key`, a deterministic per-thread `trace_id` (UUIDv5 of the thread
  key — no `thread_traces` table), and the execution span's `traceparent`, so
  codex-app-wrapper can configure codex's OTLP export and the harness's
  `session_task.turn` spans (token usage that Laminar prices into cost) join
  the execution trace. The api-rs process's own OTLP env
  (`OTEL_EXPORTER_OTLP_{ENDPOINT,TRACES_ENDPOINT,HEADERS}` and
  `OTEL_RESOURCE_ATTRIBUTES`) is always forwarded into codex sandboxes —
  the same hardcoded passthrough set the Python control plane used — so the
  Laminar ingest key flows secret → api-rs env → sandbox without touching
  values. Operator sandbox env (`SESSION_SANDBOX_EXTRA_ENV`, rendered from the
  chart's `sandbox.extraEnv`) layers on top; the endpoint host is auto-merged
  into the sandbox `NO_PROXY`, and the per-sandbox egress NetworkPolicy gets a
  namespace-scoped rule for in-cluster collector endpoints. The chart's
  `networkPolicy.otlpEgress` values open the matching api-rs egress for its
  own OTLP export on installs without a broader CNI policy.

## Goals

- Emit single-line structured logs from all Rust API binaries and library
  crates through `tracing`.
- Keep JSON stdout logs as the default local and production log path.
- Support optional OTLP trace export through standard `OTEL_*` environment
  variables.
- Centralize service metadata, environment parsing, field names, and shutdown
  flushing in one crate.
- Add metrics with bounded labels only. Runtime identifiers belong in logs and
  spans, not metric labels.
- Preserve the current crate boundaries: HTTP telemetry belongs in
  `centaur-api-server`; session and sandbox spans belong in their owning crates;
  sandbox core traits stay byte-oriented and backend-neutral.

## Non-goals

- Replacing the Python API telemetry implementation.
- Exporting OpenTelemetry logs in the first slice.
- Adding tool, workflow, final delivery, or Slack-specific telemetry to
  `api-rs` before those concepts exist there.
- Adding high-cardinality labels such as `thread_key`, `execution_id`,
  `sandbox_id`, or `user_id` to metrics.
- Making sandbox traits aware of HTTP routes, request IDs, or OpenTelemetry
  carrier formats.

## Existing State

`centaur-api-server` currently initializes JSON logs directly in `main.rs`.
There is no shared telemetry crate, HTTP request middleware, `/metrics`
endpoint, OpenTelemetry setup, or shared field contract.

The durable runtime boundaries already exist:

- `POST /api/session/{thread_key}`
- `POST /api/session/{thread_key}/messages`
- `POST /api/session/{thread_key}/execute`
- `GET /api/session/{thread_key}/events`
- `SessionRuntime::execute_session`
- `SessionRuntime::ensure_session_sandbox`
- `SandboxManager` lifecycle and I/O operations
- `PgSessionStore` session, execution, and event persistence

Those are the boundaries telemetry should describe.

## Proposed Workspace Layout

```text
services/api-rs/
  crates/
    centaur-telemetry/
      src/lib.rs
    centaur-api-server/
    centaur-session-runtime/
    centaur-session-sqlx/
    centaur-sandbox-manager/
```

## Telemetry Crate

`centaur-telemetry` owns:

- `TelemetryConfig`
- `init_telemetry`
- `TelemetryGuard`
- service/resource attributes
- shared structured field names
- exporter selection
- shutdown flushing

Configuration comes from:

| Env var | Purpose |
| ------- | ------- |
| `RUST_LOG` | Rust tracing filter, default `info` |
| `OTEL_SERVICE_NAME` | Service name, default `centaur-api-rs` |
| `CENTAUR_ENVIRONMENT` | Deployment environment |
| `DEPLOY_ENV` | Environment fallback |
| `ENVIRONMENT` | Environment fallback |
| `OTEL_TRACES_EXPORTER` | `otlp` or explicit off values |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Enables OTLP trace export when set |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Trace-specific OTLP endpoint |

The default with no OTLP endpoint is JSON logs only. That keeps local execution
simple and avoids noisy exporter failures in tests.

## Logs

Logs should use `tracing` fields instead of interpolated strings. The common
fields are:

| Field | Use |
| ----- | --- |
| `service` | Stable service name, when not already carried as resource metadata |
| `component` | Crate or subsystem, for example `session_runtime` |
| `event` | Machine-readable event name |
| `thread_key` | Thread/session identity for logs and spans only |
| `execution_id` | Execution identity for logs and spans only |
| `sandbox_id` | Sandbox identity for logs and spans only |

Do not log:

- auth headers
- API keys
- environment values that may contain secrets
- message text or raw message parts
- sandbox stdout lines
- raw metadata blobs

## Metrics

Metrics should be exported from `/metrics` in Prometheus text format first,
because that matches the existing Centaur/VictoriaMetrics operational model.
OTLP metrics can be added later behind the same telemetry crate.

Initial metric set:

| Metric | Labels |
| ------ | ------ |
| `http_server_requests_total` | `method`, `route`, `status` |
| `http_server_request_duration_seconds` | `method`, `route`, `status_class` |
| `http_server_requests_in_flight` | none |
| `centaur_session_executions_total` | `harness`, `status` |
| `centaur_session_execution_duration_seconds` | `harness`, `status` |
| `centaur_sandbox_operations_total` | `backend`, `operation`, `status` |
| `centaur_sandbox_warm_pool_claims_total` | `result` |

Label rules:

- Use route templates, not concrete paths.
- Keep labels low-cardinality and finite.
- Never label metrics with `thread_key`, `execution_id`, `sandbox_id`, or
  `user_id`.

## Traces

Trace spans should follow OpenTelemetry semantic conventions where they apply,
especially HTTP server spans. Centaur-specific attributes should use the
`centaur.*` namespace.

Initial span set:

| Span | Owner |
| ---- | ----- |
| `centaur.api_rs.http_request` | `centaur-api-server` middleware |
| `centaur.api_rs.session.create_or_get` | `centaur-session-runtime` |
| `centaur.api_rs.session.messages.append` | `centaur-session-runtime` |
| `centaur.api_rs.session.execute` | `centaur-session-runtime` |
| `centaur.api_rs.session.execution` | `centaur-session-runtime` |
| `centaur.api_rs.sandbox.ensure` | `centaur-session-runtime` |
| `centaur.api_rs.sandbox.create` | `centaur-sandbox-manager` |
| `centaur.api_rs.sandbox.open_io` | `centaur-sandbox-manager` |
| `centaur.api_rs.sandbox.write_input` | `centaur-session-runtime` |
| `centaur.api_rs.session.stdout_pump` | `centaur-session-runtime` |
| `centaur.api_rs.session.events.stream` | `centaur-session-runtime` |
| `centaur.api_rs.codex_app_server.event` | `centaur-session-runtime` |
| `centaur.api_rs.codex_app_server.tool_call` | `centaur-session-runtime` |

Spans may carry:

- `centaur.thread_key`
- `centaur.execution_id`
- `centaur.sandbox_id`
- `centaur.harness_type`
- `centaur.sandbox.backend`
- `centaur.session.event_type`
- `codex_app_server.source`
- `codex_app_server.event_type`
- `codex_app_server.item_type`
- `tool.kind`
- `tool.name`
- `tool.method`
- `tool.status`

They must not carry message text, stdout lines, raw metadata, or secrets.
Tool-call spans must not carry tool arguments, command output, tool results, or
prompt content.

## Trace Continuity

The Rust API should preserve one trace tree per execution where possible.
The current implementation explicitly propagates trace context across spawned
stdout-pump work by keeping a process-local execution span registry keyed by
`execution_id`.

Trace context needs to be persisted or explicitly propagated before spawning
background work. The stdout pump now uses the registered execution span as the
parent for `centaur.api_rs.session.stdout_pump`,
`centaur.api_rs.codex_app_server.event`, and
`centaur.api_rs.codex_app_server.tool_call`.

Cross-process continuity after an API restart remains future work. If `api-rs`
needs that, reuse `thread_traces(thread_key, trace_id, root_span_id)` when
available or add equivalent columns to `sessions`.

## Implementation Plan

1. Add `centaur-telemetry`.
   - Add the workspace crate.
   - Move JSON tracing initialization out of `centaur-api-server`.
   - Add optional OTLP trace export.
   - Add `TelemetryGuard` shutdown flushing.
   - Add tests for environment-driven exporter selection.

2. Add HTTP telemetry.
   - Add `tower-http` request tracing.
   - Add request logs, route-template metrics, and HTTP server spans.
   - Add `/metrics`.

3. Add runtime spans and logs.
   - Instrument session create, append, execute, stream, sandbox ensure, pipe
     open, input write, stdout pump, and terminal states.
   - Keep payload content out of telemetry.

4. Add metrics registry and domain metrics.
   - Use the shared `metrics` facade and Prometheus exporter for counters,
     histograms, and text exposition.
   - Add session execution lifecycle, execution duration, sandbox operation,
     and warm-pool claim metrics with bounded labels.

5. Add metric scrape readiness.
   - Add Helm scrape annotations for the `api-rs` `/metrics` endpoint.
   - Document the external Prometheus/VictoriaMetrics and Tempo/Jaeger
     requirements.
   - Document the logs datasource requirement and trace ID correlation contract.

6. Add trace continuity.
   - Propagate context into background tasks.
   - Keep execution spans active until terminal state.
   - Reuse `thread_traces` when available for cross-process continuity.
   - Persist root trace/span context if restart continuity is required.

7. Verify locally.
   - Run Rust unit tests.
   - Start `centaur-api-server` locally.
   - Exercise create, messages, execute, and events endpoints.
   - Verify JSON logs include `trace_id` and `span_id` when emitted inside a
     span.
   - With an OTLP endpoint configured, verify a trace arrives in the target
     backend.
   - With a logs backend configured, verify a log trace ID opens the same trace
     in Grafana/Jaeger.

## Acceptance Criteria

- API-RS still starts with no telemetry-specific environment variables.
- Logs remain single-line JSON by default.
- Setting `OTEL_EXPORTER_OTLP_ENDPOINT` enables trace export.
- Dropping the telemetry guard flushes pending spans.
- Metrics labels stay bounded.
- No telemetry path records message text, stdout lines, raw metadata, secrets,
  or auth headers.
