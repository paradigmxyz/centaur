"""CLI for Laminar trace investigation for Centaur agent executions."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(name="laminar", help="Laminar trace investigation for Centaur agent executions")


@app.callback()
def main() -> None:
    """laminar CLI."""


@app.command("health")
def health():
    """Assert laminar connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.health()
        if isinstance(details, dict) and not details.get("ready", False):
            raise RuntimeError(str(details.get("response") or "laminar health check failed"))
        payload = {"ok": True, "tool": "laminar", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "laminar", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
