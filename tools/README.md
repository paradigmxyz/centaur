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
