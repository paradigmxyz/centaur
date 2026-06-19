# Migrating to api-rs and slackbotv2

This document is a deployment checklist for moving an existing Centaur install
from the legacy Python API/slackbot path to the Rust control plane (`api-rs`)
and `slackbotv2`.

The migration has three goals:

- Slack events enter through `slackbotv2`.
- Sessions, messages, executions, and replayable events are owned by `api-rs`.
- Sandboxes launched by `api-rs` can still reach tools, overlays, secrets, and
  their per-sandbox iron-proxy.

## 1. Deploy the api-rs control plane

Enable api-rs and slackbotv2 in the Helm values, and disable the legacy Python
API and legacy slackbot once traffic has moved.

Typical values:

```yaml
api:
  enabled: false

apiRs:
  enabled: true
  sandboxWarmPoolSize: 1

slackbot:
  enabled: false

slackbotv2:
  enabled: true
```

Verify the live deployment, not just the merged values:

```bash
kubectl -n centaur get deploy centaur-api-rs centaur-slackbotv2
kubectl -n centaur rollout status deploy/centaur-api-rs --timeout=300s
kubectl -n centaur rollout status deploy/centaur-slackbotv2 --timeout=300s
```

If your deployment uses Argo CD or another GitOps controller, force a refresh
after changing values and wait for the application to become `Synced` and
`Healthy` before testing sessions.

## 2. Keep secrets available to all new consumers

api-rs, slackbotv2, console, and sandbox pods may not read exactly the same
environment variable names as the legacy Python API. During migration, verify
that each new workload receives the required secrets through the configured
Kubernetes Secret or secret manager integration.

Common checks:

```bash
kubectl -n centaur exec deploy/centaur-api-rs -- env | grep -E 'DATABASE_URL|IRON_CONTROL|OP_CONNECT|SLACK|OPENAI|ANTHROPIC'
kubectl -n centaur exec deploy/centaur-slackbotv2 -- env | grep -E 'SLACK|CENTAUR|API'
kubectl -n centaur exec deploy/centaur-console -- env | grep -E 'DATABASE_URL|OP_CONNECT|IRON_CONTROL'
```

After patching a Secret, restart every workload that consumes it:

```bash
kubectl -n centaur rollout restart \
  deploy/centaur-api-rs \
  deploy/centaur-slackbotv2 \
  deploy/centaur-console \
  deploy/centaur-console-worker
```

## 3. Route sandbox tool calls locally when needed

Legacy sandboxes often called tools through HTTP routes on the API service. In
api-rs-managed sandboxes, prefer the local tool shim path when a dedicated tool
server URL is not present.

The expected `call` helper behavior is:

- if `CENTAUR_TOOLS_URL` is set, call the tool server;
- otherwise, if `centaur-tools` is available, use local shims for:
  - `call tools`
  - `call discover <tool>`
  - `call <tool> <method> [json]`
- do not fall back to deprecated `/tools/...` HTTP routes on `CENTAUR_API_URL`.

From inside a sandbox, validate:

```bash
command -v centaur-tools
centaur-tools list
call tools
call discover <tool-name>
call <tool-name> <method-name> '{"example":"payload"}'
```

## 4. Make overlay tools installable as shims

Overlay tools must expose enough Python package metadata for the sandbox shim
installer to discover and run them locally.

For Python tools, include a script entry point in `pyproject.toml`:

```toml
[project.scripts]
mytool = "mytool.cli:app"
```

If the tool package is rooted at the tool directory itself, also make the wheel
builder include that package:

```toml
[tool.hatch.build.targets.wheel]
packages = ["."]
```

Tool clients should be importable as modules/packages, not only as anonymous
files. This matters for relative imports such as:

```python
from .database import Database
```

If a tool imports Centaur SDK modules from the base image, ensure the local
runner includes the Centaur root (for example `/opt/centaur`) in `PYTHONPATH`.

## 5. Apply overlay database migrations

api-rs applies only its own embedded core migrations. The legacy Python `api`
also applied an overlay's `services/api/db/migrations/*.sql` (dbmate format,
tracked in a `schema_migrations_overlay` table) whenever `CENTAUR_OVERLAY_DIR`
was set; the Rust control plane does not. Without this, overlay-owned tables are
never created and overlay workflows fail at runtime with
`relation "..." does not exist`.

api-rs restores this: **after** it runs its core migrations, it applies overlay
migrations from the repo-cache clone (so overlay schema may depend on core),
tracked in `schema_migrations_overlay` — a ledger independent of the core
`schema_migrations`, so an applied version is never re-run (an upgrade from the
legacy dbmate path skips versions it already recorded).

It is **opt-in per source**: set `migrationsSubdir` (conventionally
`services/api/db/migrations`) on an `overlays.sources` entry to enable it for that
overlay. Unlike `toolsSubdir`/`workflowsSubdir`/`skillsSubdir`, it has no default,
so the common case — sources that carry no DB migrations, including the base repo
— applies nothing and adds no startup wait.

```yaml
overlays:
  sources:
    - repo: your-org/your-overlay
      ref: main
      migrationsSubdir: services/api/db/migrations  # opt in to overlay migrations
```

When at least one source opts in (and `repoCache.enabled` + `apiRs.runMigrations`
are true), api-rs waits for the repo-cache readiness marker, then applies each
pending `-- migrate:up` section in `NNN_` filename order, recording the numeric
version. A migration body and its ledger insert commit together, so a failure
rolls back and retries on the next start. A configured source whose migrations
directory is absent at runtime is skipped, matching the overlay-subdir model.

Because overlay migrations run **after** the core migrations but as a separate,
independently-numbered set, they should be **self-contained** relative to each
other (numbered independently of core). Migration files must follow dbmate
conventions:

- A **zero-padded numeric `NNN_` prefix** (e.g. `001_…`, `010_…`) — the prefix is
  the version (files without one are skipped), and applies in lexical filename
  order, so pad consistently.
- `-- migrate:up` / `-- migrate:down` markers at the **start of a line**; the
  `migrate:up` section must be non-empty (a malformed migration fails startup
  rather than being silently recorded as applied).
- `-- migrate:up transaction:false` is honored for a migration that cannot run in
  a transaction (e.g. `CREATE INDEX CONCURRENTLY`); such a migration must contain a
  **single statement** (Postgres wraps a multi-statement string in an implicit
  transaction).

If more than one source opts in, all opted-in sources share the single
`schema_migrations_overlay` ledger keyed by version, so they must use **distinct
version prefixes** (e.g. partition the number space) — otherwise a colliding
version from a later source is treated as already applied and skipped.

Tuning (`values.yaml`, under `apiRs`):

- `applyOverlayMigrations` (default `true`) — master toggle; disables overlay
  migrations even when a source opts in.
- `overlayMigrations.readyTimeoutSeconds` (default `300`) — how long api-rs waits
  for the repo-cache readiness marker before applying (on timeout it logs and
  skips, and api-rs still starts).

Verify (the apply runs in the api-rs container, logged at startup):

```sh
kubectl -n <ns> logs <api-rs-pod> | grep overlay
kubectl -n <ns> exec deploy/<postgres> -- \
  psql "$DATABASE_URL" -c "SELECT version FROM schema_migrations_overlay ORDER BY version"
```

The applied versions and the ledger rows should match the overlay's `NNN_`
migration prefixes, and the overlay-owned tables should exist.

## 6. Verify console and per-sandbox proxies

api-rs-managed sandboxes use per-sandbox iron-proxy pods for outbound access and
secret injection. When console is enabled, the proxy's effective config is
owned by console, not by static proxy environment variables alone.

Important behavior:

- Ready warm-pool sandboxes may start under a bootstrap principal.
- A bootstrap warm proxy may have no access to secrets or Postgres upstreams.
- On warm-pool claim, api-rs reassigns the proxy to the session principal via
  console.
- Kubernetes annotations on an old proxy pod can be stale; the console
  proxy record is the source of truth.

Check a proxy's local listeners:

```bash
kubectl -n centaur exec <proxy-pod> -- sh -lc \
  '(ss -ltnp || netstat -ltnp || cat /proc/net/tcp) 2>&1 | grep -E "5432|8080|9090|443|80" || true'
```

For Postgres-backed tools, a claimed session proxy with the right grants should
eventually listen on `5432`. An idle bootstrap warm proxy may not listen on
`5432`; that is expected.

## 7. Understand the SQL-backed warm pool

The api-rs warm pool is tracked in Postgres, not only through Kubernetes
objects. Inspect it from the database used by api-rs:

```sql
select *
from session_warm_sandboxes
order by created_at desc
limit 20;
```

Expected states:

- `ready`: available for a future session with the matching workload key.
- `claimed`: already assigned to a thread key.
- `failed`: api-rs tried to claim it but found the sandbox unusable.

If you manually delete a sandbox pod during incident response, delete the
`Sandbox` custom resource too. Deleting only the pod can let the sandbox
operator recreate stale runtime state without matching proxy resources.

```bash
kubectl -n centaur delete sandbox.agents.x-k8s.io <sandbox-id> \
  --ignore-not-found --wait=false
```

## 8. Validate an end-to-end session

At minimum, validate that a fresh api-rs sandbox can:

1. start and attach;
2. list local tools;
3. discover an installed tool;
4. call a simple tool method;
5. make an outbound LLM/API request through iron-proxy;
6. stream a final answer through slackbotv2.

Useful sandbox smoke test:

```bash
kubectl -n centaur exec <sandbox-pod> -- sh -lc '
  set -e
  command -v centaur-tools
  centaur-tools list | head
  call tools | head
  call discover <tool-name> >/tmp/tool-discover.json
  call <tool-name> <method-name> '\''{"example":"payload"}'\''
'
```

For a Postgres-backed tool, also verify that the sandbox receives a DSN pointing
at its per-sandbox proxy and that the proxy is listening on the expected
Postgres port.

## 9. Common failure modes

### `401 Unauthorized` from an LLM provider

Check whether the value reaching the sandbox or proxy is a placeholder literal
instead of a real secret. This usually means the required secret is missing from
the secret manager token path, the Kubernetes Secret, or the console grant.

### `404` from `/tools/...`

The sandbox is using the deprecated API tool route. Update the `call` helper or
the sandbox image so local `centaur-tools` shims are used when
`CENTAUR_TOOLS_URL` is unset.

### Tool appears in the overlay but not in `centaur-tools list`

The overlay package likely lacks a script entry point or wheel package metadata.
Add `[project.scripts]` and, when needed, `[tool.hatch.build.targets.wheel]`.

### Tool imports fail with relative-import errors

The runner is importing `client.py` as an anonymous file module. Import the tool
as a package/module from its project directory instead.

### Postgres-backed tool gets `connection refused` on the proxy service

Check whether the sandbox is an idle bootstrap warm sandbox or a claimed session
sandbox. Idle bootstrap warm proxies may not listen on Postgres. Claimed session
proxies should be reassigned in console and then pick up the Postgres
listener after their next sync.

### Pods keep coming back after manual deletion

Delete the owning `Sandbox` custom resource, not just the Pod.
