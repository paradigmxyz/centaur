"""CLI for Snapshot."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="snapshot", help="Snapshot — off-chain governance voting, proposals, and spaces"
)


@app.callback()
def main() -> None:
    """snapshot CLI."""


@app.command("health")
def health():
    """Assert snapshot connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_spaces(first=1)
        payload = {"ok": True, "tool": "snapshot", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "snapshot", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
