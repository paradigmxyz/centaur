"""CLI for EODHD Financial API."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from .client import EodhdClient

load_dotenv()

app = typer.Typer(name="eodhd", help="EODHD CLI for equity quotes and historical prices")


@app.command("health")
def health():
    """Assert eodhd connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_quote("AAPL")
        payload = {"ok": True, "tool": "eodhd", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "eodhd", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client() -> EodhdClient:
    return EodhdClient()


@app.command()
def quote(
    symbol: str = typer.Argument(..., help="Ticker symbol (e.g. AAPL)"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get a delayed quote for a US equity."""
    data = get_client().get_quote(symbol)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('code', symbol)}[/] Quote\n")
    console.print(f"Open:      [yellow]{data.get('open')}[/]")
    console.print(f"High:      [yellow]{data.get('high')}[/]")
    console.print(f"Low:       [yellow]{data.get('low')}[/]")
    console.print(f"Close:     [yellow]{data.get('close')}[/]")
    console.print(f"Volume:    [green]{data.get('volume')}[/]")
    change_p = data.get("change_p")
    if change_p is not None:
        color = "green" if change_p >= 0 else "red"
        console.print(f"Change:    [{color}]{data.get('change')} ({change_p:+.2f}%)[/]")


@app.command()
def eod(
    symbol: str = typer.Argument(..., help="Ticker symbol (e.g. AAPL)"),
    from_date: str = typer.Option(None, "--from-date", "-f", help="Start date (YYYY-MM-DD)"),
    to_date: str = typer.Option(None, "--to-date", "-t", help="End date (YYYY-MM-DD)"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get historical daily OHLCV prices for a US equity."""
    data = get_client().get_eod_prices(symbol, from_date=from_date, to_date=to_date)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{symbol}.US[/] EOD Prices ({len(data)} days)\n")
    for row in data[-20:]:
        console.print(
            f"[dim]{row.get('date')}[/]  "
            f"O={row.get('open')}  H={row.get('high')}  "
            f"L={row.get('low')}  C=[yellow]{row.get('close')}[/]  "
            f"V={row.get('volume')}"
        )
    if len(data) > 20:
        console.print(f"\n[dim]Showing last 20 of {len(data)} days[/]")


if __name__ == "__main__":
    app()
