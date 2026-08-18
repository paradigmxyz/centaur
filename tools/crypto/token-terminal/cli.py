"""CLI for Token Terminal."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="token-terminal",
    help="Token Terminal — protocol revenue, fees, financial statements, and metrics",
)


@app.callback()
def main() -> None:
    """token-terminal CLI."""


@app.command("health")
def health():
    """Assert token-terminal connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_projects(limit=1)
        payload = {"ok": True, "tool": "token-terminal", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "token-terminal", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
