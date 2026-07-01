"""CLI for Etherscan."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="etherscan",
    help="Etherscan — Ethereum block explorer, transactions, contracts, and tokens",
)


@app.callback()
def main() -> None:
    """etherscan CLI."""


@app.command("health")
def health():
    """Assert etherscan connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_eth_price()
        payload = {"ok": True, "tool": "etherscan", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "etherscan", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
