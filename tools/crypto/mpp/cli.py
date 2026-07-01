"""CLI for Market data via MPP (Machine Payments Protocol)."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="mpp",
    help="Market data via MPP (Machine Payments Protocol) — token prices, web search, on-chain data, trending tokens, wallet balances, and Dune SQL queries. Paid per-query with Tempo stablecoins.",
)


@app.callback()
def main() -> None:
    """mpp CLI."""


@app.command("health")
def health():
    """Assert mpp connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_trending()
        payload = {"ok": True, "tool": "mpp", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "mpp", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
