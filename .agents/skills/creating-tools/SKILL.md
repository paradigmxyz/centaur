---
name: creating-tools
description: "Scaffold and build new tool integrations in tools/. Use when asked to create a new tool, add an API integration, or build a new client for an external service."
---

# Creating Tools

A tool is an independently packaged Python CLI under `tools/<category>/<name>/`
(categories: `business`, `comms`, `crypto`, `infra`, `media`, `productivity`,
`research`). api-rs reads its `pyproject.toml` for secret grants, and sandboxes
install its `[project.scripts]` entry as a CLI shim. Agents use it through
`centaur-tools list`, `<tool> --help`, and the CLI itself; there is no HTTP
tool-method API.

Read `docs/pages/extend/tools.mdx` for the full `[tool.centaur]` secret
reference. Copy the shape of a recent tool such as `tools/productivity/luma`
or `tools/research/pangram` rather than inventing a new layout.

## Layout

```
tools/<category>/<name>/
├── pyproject.toml   # metadata, [project.scripts], wheel mapping, [tool.centaur]
├── __init__.py      # one-line docstring
├── client.py        # API client class + _client() factory
├── cli.py           # thin Typer CLI over the client, with a `health` command
├── test_client.py   # client tests against httpx.MockTransport
└── test_cli.py      # CLI tests with typer.testing.CliRunner
```

## pyproject.toml

```toml
[project]
name = "<name>"
description = "<One-line description>"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = [
    "httpx>=0.27.0",
    "typer>=0.12.0",
]

[project.scripts]
<name> = "centaur_tool_<module>.cli:app"

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["."]

[tool.hatch.build.targets.wheel.sources]
"." = "centaur_tool_<module>"

[tool.centaur]
module = "client.py"
secrets = [
    {type = "http", name = "<NAME>_API_KEY", match_headers = ["Authorization"], hosts = ["api.example.com"]},
]
```

- `[project.scripts]` is required for the tool to be visible to agents. Name
  the package `centaur_tool_<module>`, where `<module>` is the tool name in
  snake_case, so it cannot shadow an installed dependency. The wheel
  `packages`/`sources` mapping must match the script's package;
  `scripts/validate_cli_packaging.py` checks this in CI.
- Declare every credential in `secrets` (or `optional_secrets`) with the
  narrowest `hosts` list. Omit `secrets` for public APIs.
- Add dependencies only when needed. Use `httpx`, never `requests`: httpx
  honors the proxy that iron-proxy relies on.

## client.py

- Get credentials with `from centaur_sdk import secret` and `secret("KEY")`. In
  a sandbox this returns a placeholder that iron-proxy swaps for the real value
  on the allowed hosts. Never use `os.getenv`/`os.environ` or `load_dotenv`;
  `tools/ruff.toml` bans them.
- One client class. Public methods are the tool's operations; prefix helpers
  with `_`. Add type hints and a docstring to each public method.
- Raise on HTTP errors (`response.raise_for_status()`) and validate response
  shape at the boundary.
- Support `close()` and the context-manager protocol, and end the module with a
  `_client()` factory.

```python
"""Client for the <Name> API."""

from __future__ import annotations

from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://api.example.com"


class <Name>Client:
    def __init__(self, api_key: str | None = None, timeout: float = 30.0) -> None:
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _request(self, method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        headers = {"Authorization": f"Bearer {self._api_key or secret('<NAME>_API_KEY')}"}
        response = self.client.request(method, f"{BASE_URL}{path}", headers=headers, **kwargs)
        response.raise_for_status()
        result = response.json()
        if not isinstance(result, dict):
            raise RuntimeError("<Name> returned an unexpected response.")
        return result

    def search(self, query: str, limit: int = 10) -> dict[str, Any]:
        """Search for items."""
        return self._request("GET", "/search", params={"q": query, "limit": limit})

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> <Name>Client:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _client() -> <Name>Client:
    return <Name>Client()
```

## cli.py

- Keep it thin: parse arguments, call one client method, and print JSON to
  stdout with `typer.echo(json.dumps(...))`.
- Catch expected errors, print a short message to stderr, and exit 1. Never
  echo response bodies or headers from failed requests; they can contain
  credentials.
- Provide `health`: make one cheap authenticated read and print
  `{"ok": bool, "tool": "<name>", "error": str | null, "details": {...}}`,
  exiting 1 when `ok` is false. The `tool-health-smoke` skill runs it on every
  tool.

```python
"""Command-line interface for the <Name> API."""

from __future__ import annotations

import json

import httpx
import typer

from .client import _client

app = typer.Typer(name="<name>", help="<Description>")


def _echo(payload: dict) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))


def _error_message(exc: Exception) -> str:
    if isinstance(exc, httpx.HTTPStatusError):
        return f"<Name> API error: HTTP {exc.response.status_code}"
    if isinstance(exc, httpx.HTTPError):
        return f"<Name> request failed ({type(exc).__name__})."
    return str(exc)


@app.command()
def health() -> None:
    """Check authentication and connectivity."""
    try:
        with _client() as client:
            details = client.search("health", limit=1)
    except (httpx.HTTPError, KeyError, RuntimeError) as exc:
        _echo({"ok": False, "tool": "<name>", "error": _error_message(exc), "details": {}})
        raise typer.Exit(1) from None
    _echo({"ok": True, "tool": "<name>", "error": None, "details": details})


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query."),
    limit: int = typer.Option(10, "--limit", "-n", help="Maximum results."),
) -> None:
    """Search for items."""
    try:
        with _client() as client:
            _echo(client.search(query, limit=limit))
    except (httpx.HTTPError, KeyError, RuntimeError) as exc:
        typer.echo(_error_message(exc), err=True)
        raise typer.Exit(1) from None


if __name__ == "__main__":
    app()
```

## Tests

Import the tool as `centaur_tool_<module>`; `.github/scripts/run-tool-tests.sh`
links the tool directory under that name before running pytest.

- Client: inject `httpx.Client(transport=httpx.MockTransport(handler))` and
  assert the method, URL, query or body, and auth header the real API expects,
  plus how responses are parsed. See `tools/research/pangram/test_client.py`.
- CLI: patch `cli._client`, invoke with `CliRunner`, and check exit codes,
  JSON output, and that a failed request does not print the response body.
- Test real behavior such as pagination, polling, and error mapping. Don't add
  tests that only restate the code.

Checks:

```bash
uvx ruff check tools/<category>/<name>
uvx ruff format --check tools/<category>/<name>
python scripts/validate_cli_packaging.py
.github/scripts/run-tool-tests.sh   # runs every tool's tests the way CI does
```

## Credentials and verification

1. Store the secret in the deployment's configured secret source under the
   exact `name` from `pyproject.toml` (see `docs/pages/secrets/`).
2. Grant it: `centaur-perms ... principals grant <principal> --tool <name>`
   registers the declared secrets, creates the `tool-<slug>` role, and assigns
   it. See "Register and Grant a Tool" in
   `docs/pages/secrets/advanced-permissioning.mdx`.
3. From a fresh local sandbox, run `centaur-tools list`, `<name> --help`,
   `<name> health`, and one real command. Configuration alone does not prove
   access works.

If the tool is missing, check the `[project.scripts]` entry, the wheel mapping,
`[tool.centaur] module = "client.py"`, and the configured `TOOL_DIRS` or
overlay `toolsSubdir`. Add organization-specific tools to an overlay repo
instead of this one (`docs/pages/extend/overlay.mdx`).
