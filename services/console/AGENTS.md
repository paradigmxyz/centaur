# Console Guide

## Role

The console is a Rails application that provides the operator UI and the
credential-control JSON API. It manages principals, roles, grants, encrypted
secret records, proxy synchronization, broker credentials, console login, and
MCP OAuth flows. Its Threads surface reads Centaur session data and is not a
second session control plane.

Use `README.md` and `docs/API.md` for the supported behavior and API shapes.

## Invariants

- Keep cookie-backed console sessions, bearer-authenticated operator APIs,
  proxy sync authentication, and OAuth/MCP tokens as separate trust boundaries.
- Secret plaintext may be accepted for creation or rotation but must never be
  returned, logged, rendered, inspected in tests, or stored outside encrypted
  model attributes and configured providers.
- Apply authorization in controllers and service objects before loading or
  mutating scoped resources. Test disabled users, non-admin users, namespace
  isolation, token replay, and ownership checks where relevant.
- OAuth/MCP changes must cover redirect validation, consent, PKCE, refresh-token
  family rotation and replay, revocation, account disablement, and identity
  reconciliation. A connected UI state alone is not proof of usable access.
- The Threads UI is an observer of durable session data. Do not make it write
  chat messages or bypass the session API.
- Put business logic in models or `app/services`, keep controllers thin, and
  preserve JSON error and pagination contracts.
- Generate migrations with Rails and commit the resulting `db/schema.rb` change.
  Do not hand-edit the schema dump.
- Use local fixtures or synthetic snapshots for UI work; do not make tests
  depend on a remote database.

## Local database

Console requires ParadeDB because its schema and skill search use the
`pg_search` extension; stock Postgres is not sufficient. For local development,
run ParadeDB in Docker from `services/console`:

```bash
just paradedb
```

This starts the pinned ParadeDB image on `127.0.0.1:55432` and keeps its data in
a named Docker volume. `just dev` starts the same container automatically. When
using an existing ParadeDB server, configure the `CENTAUR_CONSOLE_DB_*`
variables and set `CENTAUR_CONSOLE_MANAGE_PARADEDB=false`; the server must make
`pg_search` available to the Console database.

## Validation

From `services/console`, point Rails at the Docker ParadeDB instance before
running database tasks or tests:

```bash
export CENTAUR_CONSOLE_DB_HOST=127.0.0.1
export CENTAUR_CONSOLE_DB_PORT=55432
export CENTAUR_CONSOLE_DB_USERNAME=postgres
export CENTAUR_CONSOLE_DB_PASSWORD=postgres

just paradedb
bundle install
bin/rails db:prepare
bin/rails test
bin/rubocop
bin/brakeman --quiet --no-pager --exit-on-warn --exit-on-error
```

`bin/ci` runs the full local CI sequence, including dependency audits, tests,
and seed validation. Add focused controller/model/service tests with each
behavior change. For visible UI changes, run `just dev`, exercise the affected
flow in a browser, and check narrow and wide layouts plus keyboard/focus states.
