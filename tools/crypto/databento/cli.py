"""CLI for Databento Historical API."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

load_dotenv()

app = typer.Typer(name="databento", help="Databento Historical API — stock market OHLCV data")


@app.command("health")
def health():
    """Assert databento connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_stock_prices("AAPL", "2024-01-03", "2024-01-04")
        payload = {"ok": True, "tool": "databento", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "databento", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


@app.command()
def prices(
    symbol: str = typer.Option(..., "--symbol", "-s", help="Ticker symbol (e.g., AAPL)"),
    start: str = typer.Option(..., "--start", help="Start date (YYYY-MM-DD)"),
    end: str = typer.Option(..., "--end", help="End date (YYYY-MM-DD)"),
    schema: str = typer.Option(
        "ohlcv-1d", "--schema", help="Data schema (ohlcv-1d, ohlcv-1m, ohlcv-1h)"
    ),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get OHLCV stock prices.

    Examples:
        databento prices --symbol AAPL --start 2026-01-01 --end 2026-01-31
        databento prices --symbol AAPL --start 2026-01-01 --end 2026-01-31 --schema ohlcv-1m
    """
    from .client import DatabentoClient

    try:
        client = DatabentoClient()
        data = client.get_stock_prices(symbol, start, end, schema)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No price data found.[/]")
        raise typer.Exit()

    console.print(f"[green]{len(data)} records for {symbol}[/]\n")
    print(json.dumps(data, indent=2))


if __name__ == "__main__":
    app()
