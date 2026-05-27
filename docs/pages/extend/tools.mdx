---
title: Creating Tools
description: Add Centaur tool plugins with client.py, pyproject metadata, and typed secret declarations.
---

# Creating Tools

Tools are Python plugins that Centaur discovers at API startup and exposes as
REST endpoints at `/tools/{name}/{method}`. Put organization-specific tools in
an overlay repo under `tools/` so the base Centaur repo stays generic. See
[Using an overlay](/extend/overlay) for packaging, mount paths, and chart
configuration.

Tools are loaded from `TOOL_DIRS`. In an overlay deployment, the tool must exist
under `/app/overlay/org/tools` in the API container. Later tool directories can
shadow earlier tools with the same name, so an overlay can replace a base tool
intentionally.

## Define metadata

Each tool needs `pyproject.toml` with a `[tool.centaur]` block:

```toml
[project]
name = "warehouse"
description = "Internal warehouse queries"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = ["httpx>=0.27.0"]

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

## Verify

After deploy:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api -- \
  curl -fsS http://localhost:8000/health/tools | jq
```

Check that the tool appears and that missing-secret warnings match what you
expect. If a tool is missing, inspect the overlay image contents, `TOOL_DIRS`,
the tool directory name, and `[tool.centaur] module = "client.py"`.
