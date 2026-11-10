"""CLI for Demo tool for testing CD hot."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(name="demo", help="Demo tool for testing CD hot-reload")


@app.callback()
def main() -> None:
    """demo CLI."""


@app.command("health")
def health():
    """Assert demo connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.ping()
        payload = {"ok": True, "tool": "demo", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "demo", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
