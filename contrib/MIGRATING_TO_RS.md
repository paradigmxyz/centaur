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

The expected direct CLI behavior is:

- list installed tools with `centaur-tools list`;
- discover one tool with `<tool> --help`;
- invoke tools through their direct CLI, such as `<tool> ...`;
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

## 5. Verify console and per-sandbox proxies

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

## 6. Understand the SQL-backed warm pool

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

## 7. Validate an end-to-end session

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

## 8. Common failure modes

### `401 Unauthorized` from an LLM provider

Check whether the value reaching the sandbox or proxy is a placeholder literal
instead of a real secret. This usually means the required secret is missing from
the secret manager token path, the Kubernetes Secret, or the console grant.

### `404` from `/tools/...`

The sandbox is using the deprecated API tool route. Update the tool invocation
path or the sandbox image so local direct CLI shims are used.

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
