# Tools

Drop tool directories here. Each tool needs:

```
tools/
  my-tool/
    pyproject.toml   # [tool.centaur] section with module path
    .env.example     # Document required secrets
    __init__.py
    client.py        # API client class + _client() factory
    cli.py           # typer CLI for standalone use
```

## Writing a tool

```python
# client.py
from centaur.tool_sdk import secret


class MyClient:
    def search(self, query: str, limit: int = 10) -> dict:
        """Search something."""
        token = secret("MY_API_TOKEN")
        # ... use token, return results ...
        return {"results": [...]}


def _client() -> MyClient:
    return MyClient()
```

## Secrets

Secrets are resolved in this order:
1. **Tool `.env`** — per-tool overrides in `tools/<name>/.env`
2. **Root `.env`** — central file at repo root (define all secrets here)
3. **Environment variables** — for Docker, k8s, sops, 1Password, etc.

Use `secret("KEY")` to access. Never use `os.environ` — tool secrets are scoped.

## Available Plugins

The open-source tool inventory lives in this `tools/` tree and changes over time. To see what ships in the current repo, inspect the directories here or run Centaur and call `call tools` from a sandbox session.

Private deployments may mount additional overlay tool directories, so a running Centaur instance can expose more tools than are present in this repo.

## Sandbox Tool Paths

Sandbox startup accepts `TOOLS_PATH` and `TOOLS_OVERLAY_PATH` and appends them to
`TOOL_DIRS` for `centaur-tools` discovery. Pass them from the API process with
`SESSION_SANDBOX_PASSTHROUGH_ENV=TOOLS_PATH,TOOLS_OVERLAY_PATH`.

`TOOLS_PATH` should point at the base mounted tool directory. `TOOLS_OVERLAY_PATH`
is appended after it so overlay tools can replace base tools with the same name.

For repo-cache overlays, set `REPOS_PATH` on the API process to the host path
where the repo-cache syncs repositories. The sandbox mounts that path read-only
at `/home/agent/github`, so overlay tools can come from the cached repo:

```bash
REPOS_PATH=/var/lib/centaur/repos
TOOLS_PATH=/home/agent/github/paradigmxyz/centaur/tools
TOOLS_OVERLAY_PATH=/home/agent/github/acme/centaur-overlay/tools
SESSION_SANDBOX_PASSTHROUGH_ENV=TOOLS_PATH,TOOLS_OVERLAY_PATH
```

The repo-cache is responsible for syncing `acme/centaur-overlay`; sandbox
startup only points `centaur-tools` at the already-mounted source tree.
