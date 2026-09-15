"""Thin command-line wrapper for Artemis market data."""

import json
import math

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

from artemis import APIError, APIStatusError

from .client import METRICS, _client

load_dotenv()

app = typer.Typer(name="artemis", help="Artemis crypto market data.")
console = Console()
ERROR_EXIT_CODE = 1
TABLE_HEADERS = ("Symbol", "Price (USD)", "24h change (fraction)")
HEALTH_SYMBOLS = ("btc", "eth")


def _error_message(exc: Exception) -> str:
    if isinstance(exc, APIStatusError):
        return f"Artemis API error: HTTP {exc.status_code}"
    if isinstance(exc, APIError):
        return f"Artemis request failed ({type(exc).__name__})."
    if isinstance(exc, json.JSONDecodeError):
        return "Artemis returned invalid JSON."
    return str(exc)


@app.command()
def health() -> None:
    """Check upstream access with a small read-only quote request."""
    details = {}
    try:
        with _client() as client:
            details = client.get_market_data(list(HEALTH_SYMBOLS))
        assets = details["data"]["symbols"]
        values = [
            assets.get(symbol, {}).get(metric) if isinstance(assets.get(symbol), dict) else None
            for symbol in HEALTH_SYMBOLS
            for metric in METRICS
        ]
        if any(
            isinstance(value, bool)
            or not isinstance(value, (int, float))
            or not math.isfinite(value)
            for value in values
        ):
            raise RuntimeError("Artemis returned missing or invalid market data.")
        payload = {"ok": True, "tool": "artemis", "error": None, "details": details}
    except (APIError, RuntimeError, ValueError) as exc:
        print(
            json.dumps(
                {"ok": False, "tool": "artemis", "error": _error_message(exc), "details": details}
            )
        )
        raise typer.Exit(ERROR_EXIT_CODE) from None

    print(json.dumps(payload, indent=2, allow_nan=False))


@app.command()
def market_data(
    symbols: list[str] = typer.Argument(..., help="Asset tickers, e.g. BTC ETH."),  # noqa: B008
    json_output: bool = typer.Option(False, "--json", help="Output JSON."),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output a Markdown table."),
) -> None:
    """Fetch market data: latest USD prices and rolling 24-hour percentage changes."""
    try:
        with _client() as client:
            data = client.get_market_data(symbols)
    except (APIError, RuntimeError, ValueError) as exc:
        console.print(_error_message(exc), style="red", markup=False)
        raise typer.Exit(ERROR_EXIT_CODE) from None

    if json_output:
        print(json.dumps(data, indent=2, allow_nan=False))
        return

    rows = []
    for symbol, metrics in data["data"]["symbols"].items():
        values = (
            [metrics.get(metric) for metric in METRICS]
            if isinstance(metrics, dict)
            else [metrics for _ in METRICS]
        )
        rows.append([symbol, *(str(value) if value is not None else "N/A" for value in values)])

    if markdown:
        print("| " + " | ".join(TABLE_HEADERS) + " |")
        print("| " + " | ".join("---" for _ in TABLE_HEADERS) + " |")
        for row in rows:
            print("| " + " | ".join(row) + " |")
        return

    table = Table(*TABLE_HEADERS, title="Artemis market data")
    for row in rows:
        table.add_row(*row)
    console.print(table)


if __name__ == "__main__":
    app()
