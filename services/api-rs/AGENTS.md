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
- `centaur-workflows`: durable workflow scheduling and state, built on the
  shared `crates/absurd-sdk` library.
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

Core migrations must run on stock PostgreSQL with pgvector. Keyword-search
indexes belong to exactly one text-search backend per database, under
`crates/centaur-session-sqlx/search-migrations/{paradedb,postgres}`. Backend
migrations share the core version sequence and are merged into it by version.
Every backend migration needs a counterpart with the same version in each
backend directory; add a no-op migration where a backend has nothing to change.
`.github/scripts/check-migration-order.sh` enforces the numbering, and
`tests/migrations.rs` covers fresh installs and legacy BM25 databases.

Database-backed tests skip when their URL is absent. Point these variables at a
disposable Postgres as required by the packages you run:

- `SESSION_RUNTIME_TEST_DATABASE_URL`: session SQLx, runtime, and warm-pool
  tests; the SQLx RLS integration tests also accept it as a fallback.
- `SESSION_SQLX_TEST_DATABASE_URL`: SQLx RLS integration tests specifically.
- `ABSURD_TEST_DATABASE_URL`: top-level `crates/absurd-sdk` database tests;
  initialize the database with the Absurd schema before running them.

Tests that share `SESSION_RUNTIME_TEST_DATABASE_URL` migrate it with the
`postgres` text-search backend, which works on stock PostgreSQL with pgvector.
A database first migrated with ParadeDB BM25 indexes (including one from before
the backends split) fails with `Bm25IndexesPresent`; recreate it. SQLx tests
that create their own databases also exercise `paradedb` when `pg_search` is
available.

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

The shared Absurd SDK is validated separately from the API workspace:

```bash
cargo test --manifest-path ../../crates/absurd-sdk/Cargo.toml
```

Sandbox lifecycle, session handoff, and harness selection are covered end to
end by the repository's `e2e/` suite (`e2e/stack.sh up`, then
`e2e/stack.sh test`), which runs the chart on a dedicated Kind cluster.

For an API contract or runtime change, also build the API image, deploy to the local
stack, drive a real session through create/append/execute/events, and verify the
durable rows and terminal event. Use explicit contexts for any Kind command so
an ambient Kubernetes context cannot redirect a destructive operation.
