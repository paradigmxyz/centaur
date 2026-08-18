"""CLI for Tokenomist."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="tokenomist",
    help="Tokenomist — token unlock schedules, vesting, emissions, and allocations",
)


@app.callback()
def main() -> None:
    """tokenomist CLI."""


@app.command("health")
def health():
    """Assert tokenomist connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_tokens(limit=1)
        payload = {"ok": True, "tool": "tokenomist", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "tokenomist", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
