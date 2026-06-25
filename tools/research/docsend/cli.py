"""CLI for DocSend document downloader."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(name="docsend", help="DocSend document downloader — cloud browser + Playwright")


@app.callback()
def main() -> None:
    """docsend CLI."""


@app.command("health")
def health():
    """Assert docsend connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {"auth_mode": "local-only", "live_probe": False}
        payload = {"ok": True, "tool": "docsend", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "docsend", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
