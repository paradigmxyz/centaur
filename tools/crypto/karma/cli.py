"""CLI for Karma."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="karma",
    help="Karma — DAO delegate reputation, contributor scores, and governance analytics",
)


@app.callback()
def main() -> None:
    """karma CLI."""


@app.command("health")
def health():
    """Assert karma connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_daos()
        payload = {"ok": True, "tool": "karma", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "karma", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
