"""CLI for Tally."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(name="tally", help="Tally — on-chain governance proposals, voting, and delegates")


@app.callback()
def main() -> None:
    """tally CLI."""


@app.command("health")
def health():
    """Assert tally connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_organizations(limit=1)
        payload = {"ok": True, "tool": "tally", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "tally", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
