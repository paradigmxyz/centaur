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

Each tool needs `pyproject.toml` with a `[tool.ai-v2]` block:

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

[tool.ai-v2]
module = "client.py"
secrets = [
    {type = "http", name = "WAREHOUSE_API_KEY", match_headers = ["Authorization"], hosts = ["warehouse.internal.example.com"]},
]
```

Each entry in `secrets` declares one credential the tool can request with
`secret(...)`. The fields tell iron-proxy what to swap and where:

- `type = "http"` is the common case: an HTTP credential injected into outbound
  requests. Other types include `oauth_token`, `gcp_auth`, and `pg_dsn` for
  upstreams that need OAuth, Google service-account auth, or proxied Postgres.
- `name` is the placeholder string the sandbox sees and what
  `secret("...")` looks up.
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
the tool directory name, and `[tool.ai-v2] module = "client.py"`.
