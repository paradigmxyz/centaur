# api-rs Guide

## Role

`api-rs` is the Rust control plane. It owns durable sessions and events,
sandbox assignment and recovery, execution serialization, workflow state,
service authentication, and control-plane telemetry. Postgres is the source of
truth; process-local maps and attach streams are recoverable caches.

Important crate boundaries:

- `centaur-api-server`: HTTP routes, middleware, startup, health, and metrics.
- `centaur-session-core`: shared session types and backend-neutral contracts.
- `centaur-session-runtime`: orchestration, execution, recovery, and lifecycle.
- `centaur-session-sqlx`: persistence and embedded SQLx migrations.
- `centaur-sandbox-*`: backend-neutral sandbox contract and implementations.
- `centaur-workflows` and `absurd-sdk`: durable workflow scheduling and state.
- `centaur-iron-control`, `centaur-iron-proxy`, and `centaur-perms`: credential
  control-plane integration and authorization resources.
- `centaur-telemetry`: shared tracing and metrics support.

Read the relevant RFC under `rfcs/` before changing a core protocol.

## Invariants

- The session flow remains create/reuse -> append messages -> execute -> replay
  events. Persist state transitions before reporting them to clients.
- `input_lines` are opaque, single-line NDJSON strings at the API boundary.
  Add trace/session context without teaching the control plane every harness's
  input format; harness-specific translation belongs in the runtime adapter.
- Execution idempotency, per-session serialization, cancellation, leases, and
  terminal events must remain correct across retries and process restarts.
- New durable state belongs in Postgres, with repository methods and recovery
  tests. Do not introduce a process-local source of truth.
- Keep ingress/platform behavior out of the API. Keep Kubernetes-specific code
  behind sandbox backend interfaces.
- Authorization must be checked at the resource boundary. A valid token alone
  is not proof that the caller may read another session, tool, or file.
- Logs and durable events must not contain bearer tokens, secret values, or raw
  credential material.

## Database changes

Migrations live in `crates/centaur-session-sqlx/migrations` and are embedded in
the binary and tests. Add the next numbered SQL file; never edit or reorder an
applied migration. Update SQLx repository code and add database-backed coverage
for upgrade, read/write, and recovery behavior.

Database-backed tests skip when their URL is absent. Point these variables at a
disposable Postgres as required by the packages you run:

- `SESSION_RUNTIME_TEST_DATABASE_URL`: session SQLx, runtime, and warm-pool
  tests; the SQLx RLS integration tests also accept it as a fallback.
- `SESSION_SQLX_TEST_DATABASE_URL`: SQLx RLS integration tests specifically.
- `ABSURD_TEST_DATABASE_URL`: `absurd-sdk` database tests.

Do not report full database coverage from `cargo test --workspace` unless the
relevant variables were set and the database-backed tests actually ran.

## Validation

From `services/api-rs`:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

During iteration, prefer a focused package/test first, for example:

```bash
cargo test -p centaur-session-runtime
cargo test -p centaur-session-sqlx
cargo test -p centaur-workflows
```

Sandbox backend invariants have a local Kind suite. Prepare the cluster and
images with the `kind-e2e-*` recipes, then run all integration test binaries
(the older `e2e-kind` wrapper names a removed test target):

```bash
just kind-e2e-up
just kind-e2e-build-images
KIND_E2E_FORCE_IMAGE_LOAD=1 just kind-e2e-load-images
SANDBOX_E2E_IMPLS=all \
SANDBOX_E2E_K8S_CONTEXT=kind-centaur-api-rs-e2e \
SANDBOX_E2E_K8S_NAMESPACE=centaur-sandbox-e2e \
cargo test -p centaur-sandbox-e2e --tests -- --ignored --nocapture
```

For an API contract or runtime change, also build the API image, deploy to the local
stack, drive a real session through create/append/execute/events, and verify the
durable rows and terminal event. Use explicit contexts for any Kind command so
an ambient Kubernetes context cannot redirect a destructive operation.
