"""CLI for Normalize mixed raw invest inputs into a clean context pack."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="invest-intake", help="Normalize mixed raw invest inputs into a clean context pack"
)


@app.callback()
def main() -> None:
    """invest-intake CLI."""


@app.command("health")
def health():
    """Assert invest-intake connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        client._load_archiver_client()
        details = {"auth_mode": "local-only", "live_probe": False}
        payload = {"ok": True, "tool": "invest-intake", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "invest-intake", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
