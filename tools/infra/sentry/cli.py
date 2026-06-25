"""CLI for Sentry issues."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="sentry",
    help="Sentry issues — list/search issues, issue details, events, stacktraces, and tag values (read-only)",
)


@app.callback()
def main() -> None:
    """sentry CLI."""


@app.command("health")
def health():
    """Assert sentry connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_organizations()
        payload = {"ok": True, "tool": "sentry", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "sentry", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
