"""CLI for Postgres."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(name="investmemos", help="Postgres-backed investment memo search and retrieval")


@app.callback()
def main() -> None:
    """investmemos CLI."""


@app.command("health")
def health():
    """Assert investmemos connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_memos(limit=1)
        if isinstance(details, dict) and details.get("status") == "error":
            raise RuntimeError(str(details.get("error") or "investmemos health check failed"))
        payload = {"ok": True, "tool": "investmemos", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "investmemos", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
