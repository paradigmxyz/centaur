---
title: Creating Tools
description: Add Centaur tool plugins with client.py, pyproject metadata, and typed secret declarations.
---

# Creating Tools

Tools are Python plugins that Centaur discovers from ordered tool directories.
api-rs reads their metadata for secret grants, while agent sandboxes install
their `[project.scripts]` entries as local CLI shims. Agents use
`centaur-tools list`, `<tool> --help`, and the direct tool CLI; api-rs does not
serve legacy HTTP tool-method routes as the current sandbox registry. Put
organization-specific tools in an overlay repo under `tools/` so the base
Centaur repo stays generic. See [Using an overlay](/extend/overlay) for
packaging, mount paths, and chart configuration.

Tools are loaded from `TOOL_DIRS`. In an overlay deployment, the tool must exist
under the source's `toolsSubdir` — by default `tools/` — in its repo-cache
checkout, for example
`/var/lib/centaur/repos/your-org/centaur-overlay/tools` in the API container.
Later tool directories can shadow earlier tools with the same name, so an
overlay can replace a base tool intentionally. Sources without a tools
directory are skipped.

See the [Tool Directory](/reference/tool-directory) for the integrations that
ship with Centaur.

## Define metadata

Each tool needs `pyproject.toml` with a `[tool.centaur]` block:

```toml
[project]
name = "warehouse"
description = "Internal warehouse queries"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = ["httpx>=0.27.0"]

[project.scripts]
warehouse = "warehouse.cli:app"

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.centaur]
module = "client.py"
secrets = [
    {type = "http", name = "WAREHOUSE_API_KEY", match_headers = ["Authorization"], hosts = ["warehouse.internal.example.com"]},
]
```

Each entry in `secrets` declares one credential the tool can request with
`secret(...)`. The fields tell iron-proxy what to swap and where:

- `type = "http"` is the common case: an HTTP credential injected into outbound
  requests. Replace-mode HTTP secrets give the tool a placeholder from
  `secret("...")`; iron-proxy swaps that placeholder for the real value at the
  network boundary.
- `type = "oauth_token"` is for OAuth2 APIs. iron-proxy resolves the declared
  `fields`, runs a `refresh_token`, `client_credentials`, `password`, or
  `jwt_bearer` exchange, caches and refreshes the access token, then injects
  `Authorization: Bearer ...` for the configured `hosts`. Set
  `token_endpoint_headers` to send extra headers on the token POST itself (for
  endpoints that require an API key alongside the standard form-body client
  auth). For `jwt_bearer` (RFC 7523), supply `issuer`, `subject`, and
  `private_key` (an RSA PEM) in `fields`, plus a top-level `audience`; an
  optional `private_key_id` field is emitted as the JWT `kid` header.
- `type = "brokered_token"` routes OAuth2 refresh-token rotation through
  iron-token-broker instead of iron-proxy. Use this when the upstream IdP
  rotates refresh tokens with strict reuse detection (OpenAI Codex, Anthropic
  Claude Code OAuth, modern Okta or Auth0 with rotation enabled) and more
  than one proxy shares the credential. Required `fields`: `client_id`,
  `refresh_token`. Optional: `client_secret`. The `refresh_token` field names
  the writable credential blob the broker rewrites on every rotation; the
  other fields are read-only. Read-side fields and `token_endpoint_headers`
  entries accept `json_key` to pluck a value out of a JSON-encoded secret;
  the `refresh_token` field does not (the broker rewrites the whole
  document).
- `type = "gcp_auth"` is for Google service-account JSON. iron-proxy resolves
  the keyfile, mints Google OAuth tokens for `scopes`, and injects them for the
  configured Google API `hosts`. If omitted, hosts default to
  `*.googleapis.com` and scopes default to `cloud-platform`.
- `type = "pg_dsn"` is for Postgres. iron-proxy resolves the real upstream DSN,
  while the sandbox gets a local proxy DSN in an environment variable named by
  `name`; `database` must match the upstream database name.
- `name` is the placeholder string the sandbox sees and what
  `secret("...")` looks up for replace-mode HTTP secrets.
- `match_headers`, `match_query`, or `match_path` tell iron-proxy where in the
  request the placeholder is allowed to appear. At least one is required.
- `hosts` is the upstream allowlist for this secret. iron-proxy will only
  inject the real value on requests to these hosts.

Use `optional_secrets` for credentials the tool can run without.

## Write the client

`client.py` exports a `_client()` factory. Public methods on the returned object
become tool methods.

```python
import httpx
from centaur_sdk.tool_sdk import secret


class WarehouseClient:
    def query(self, sql: str) -> dict:
        token = secret("WAREHOUSE_API_KEY", "")
        response = httpx.post(
            "https://warehouse.internal.example.com/query",
            headers={"authorization": f"Bearer {token}"},
            json={"sql": sql},
            timeout=30,
        )
        response.raise_for_status()
        return response.json()


def _client() -> WarehouseClient:
    return WarehouseClient()
```

Do not call `load_dotenv()` in `client.py`. Server-side tools should use
`secret("KEY")`; standalone CLIs may load local `.env` files in their CLI
wrapper.

## Write the CLI

The sandbox shim installer only exposes tools with `[project.scripts]`. Keep
the CLI thin: parse command-line arguments, call the client, and print JSON or
plain text that an agent can read.

```python
import json
import typer

from .client import _client

app = typer.Typer()


@app.command()
def query(sql: str) -> None:
    print(json.dumps(_client().query(sql)))
```


## Verify

After deploy, verify from a fresh sandbox:

```bash
kubectl exec -n centaur-system <agent-sandbox-pod> -- centaur-tools list
kubectl exec -n centaur-system <agent-sandbox-pod> -- warehouse --help
kubectl exec -n centaur-system <agent-sandbox-pod> -- warehouse query "select 1"
```

Check that the tool appears, the CLI help is useful, and a real invocation
works through iron-proxy when credentials are needed. If a tool is missing,
inspect the configured repo/ref in repo-cache, `TOOL_DIRS`, the tool directory
name, `[tool.centaur] module = "client.py"`, and the `[project.scripts]` entry.
For workflow-only use, also run a small workflow that exercises
`ctx.call_tool(...)`, which uses the generated `centaur-tools call` bridge.
