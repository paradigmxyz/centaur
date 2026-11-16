"""CLI for Kalshi prediction market API."""

import json
import time

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="kalshi", help="Kalshi prediction market CLI for market data and analytics")


@app.command("health")
def health():
    """Assert kalshi connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_markets(limit=1)
        payload = {"ok": True, "tool": "kalshi", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "kalshi", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import KalshiClient

    return KalshiClient()


def format_price(cents: int | None) -> str:
    """Format price in cents to dollars/percentage."""
    if cents is None:
        return "N/A"
    return f"{cents}¢"


def format_volume(volume: int | None) -> str:
    """Format volume with K/M suffixes."""
    if volume is None:
        return "N/A"
    if volume >= 1_000_000:
        return f"{volume / 1_000_000:.1f}M"
    if volume >= 1_000:
        return f"{volume / 1_000:.1f}K"
    return str(volume)


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def markets(
    status: str = typer.Option(
        "open", "--status", "-s", help="Status filter (open/closed/settled)"
    ),
    event: str = typer.Option(None, "--event", "-e", help="Filter by event ticker"),
    series: str = typer.Option(None, "--series", help="Filter by series ticker"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List active prediction markets."""
    client = get_client()
    try:
        data = client.list_markets(
            status=status if status != "all" else None,
            event_ticker=event,
            series_ticker=series,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    markets_list = data.get("markets", [])

    if json_output:
        print(json.dumps(markets_list, indent=2))
        return

    if not markets_list:
        console.print("[yellow]No markets found[/]")
        return

    if markdown:
        rows = []
        for m in markets_list:
            yes_price = m.get("yes_ask") or m.get("last_price")
            rows.append(
                [
                    m.get("ticker", ""),
                    m.get("title", "")[:50],
                    format_price(yes_price),
                    format_volume(m.get("volume")),
                    m.get("status", ""),
                ]
            )
        print_markdown_table(["Ticker", "Title", "Yes Price", "Volume", "Status"], rows)
        return

    table = Table(title="Kalshi Markets")
    table.add_column("Ticker", style="cyan", max_width=25)
    table.add_column("Title", style="white", max_width=40)
    table.add_column("Yes", style="green", justify="right")
    table.add_column("Volume", style="yellow", justify="right")
    table.add_column("Status", style="dim")

    for m in markets_list:
        yes_price = m.get("yes_ask") or m.get("last_price")
        table.add_row(
            m.get("ticker", ""),
            m.get("title", "")[:40],
            format_price(yes_price),
            format_volume(m.get("volume")),
            m.get("status", ""),
        )

    console.print(table)


@app.command()
def market(
    ticker: str = typer.Argument(..., help="Market ticker (e.g., KXBTC-24DEC31-99999)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details for a specific market."""
    client = get_client()
    try:
        data = client.get_market(ticker)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    m = data.get("market", data)

    if json_output:
        print(json.dumps(m, indent=2))
        return

    console.print(f"\n[bold cyan]{m.get('title', ticker)}[/]")
    console.print(f"[dim]Ticker: {m.get('ticker')} | Event: {m.get('event_ticker', 'N/A')}[/]\n")

    yes_ask = m.get("yes_ask")
    yes_bid = m.get("yes_bid")
    no_ask = m.get("no_ask")
    no_bid = m.get("no_bid")
    last_price = m.get("last_price")

    console.print("[bold]Prices:[/]")
    console.print(
        f"  Yes: [green]Bid {format_price(yes_bid)}[/] / [red]Ask {format_price(yes_ask)}[/]"
    )
    console.print(
        f"  No:  [green]Bid {format_price(no_bid)}[/] / [red]Ask {format_price(no_ask)}[/]"
    )
    console.print(f"  Last: {format_price(last_price)}")

    console.print("\n[bold]Stats:[/]")
    console.print(f"  Volume: {format_volume(m.get('volume'))}")
    console.print(f"  Open Interest: {format_volume(m.get('open_interest'))}")
    console.print(f"  Status: {m.get('status', 'N/A')}")

    if m.get("result"):
        console.print(f"\n[bold]Result:[/] {m.get('result')}")


@app.command()
def trades(
    ticker: str = typer.Argument(..., help="Market ticker"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get recent trades for a market."""
    client = get_client()
    try:
        data = client.get_trades(ticker=ticker, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    trades_list = data.get("trades", [])

    if json_output:
        print(json.dumps(trades_list, indent=2))
        return

    if not trades_list:
        console.print(f"[yellow]No trades found for {ticker}[/]")
        return

    if markdown:
        rows = []
        for t in trades_list:
            ts = t.get("created_time", "")
            if ts:
                ts = ts[:19].replace("T", " ")
            rows.append(
                [
                    ts,
                    format_price(t.get("yes_price")),
                    str(t.get("count", "")),
                    t.get("taker_side", ""),
                ]
            )
        print_markdown_table(["Time", "Price", "Quantity", "Side"], rows)
        return

    table = Table(title=f"Recent Trades: {ticker}")
    table.add_column("Time", style="dim")
    table.add_column("Price", style="green", justify="right")
    table.add_column("Qty", style="yellow", justify="right")
    table.add_column("Side", style="cyan")

    for t in trades_list:
        ts = t.get("created_time", "")
        if ts:
            ts = ts[:19].replace("T", " ")
        side = t.get("taker_side", "")
        side_color = "green" if side == "yes" else "red" if side == "no" else "white"
        table.add_row(
            ts,
            format_price(t.get("yes_price")),
            str(t.get("count", "")),
            f"[{side_color}]{side}[/]",
        )

    console.print(table)


@app.command()
def history(
    ticker: str = typer.Argument(..., help="Market ticker"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days of history"),
    interval: int = typer.Option(
        1440, "--interval", "-i", help="Candle interval in minutes (1, 60, 1440)"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get price history (candlesticks) for a market."""
    client = get_client()

    try:
        market_data = client.get_market(ticker)
    except RuntimeError as e:
        console.print(f"[red]Error fetching market: {e}[/]")
        raise typer.Exit(1)

    m = market_data.get("market", market_data)
    series_ticker = m.get("series_ticker")

    if not series_ticker:
        console.print("[red]Cannot determine series ticker for this market[/]")
        raise typer.Exit(1)

    end_ts = int(time.time())
    start_ts = end_ts - (days * 86400)

    try:
        data = client.get_candlesticks(
            series_ticker=series_ticker,
            ticker=ticker,
            start_ts=start_ts,
            end_ts=end_ts,
            period_interval=interval,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    candles = data.get("candlesticks", [])

    if json_output:
        print(json.dumps(candles, indent=2))
        return

    if not candles:
        console.print(f"[yellow]No price history found for {ticker}[/]")
        return

    if markdown:
        rows = []
        for c in candles[-20:]:
            ts = c.get("end_period_ts", 0)
            date_str = time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)) if ts else ""
            rows.append(
                [
                    date_str,
                    format_price(c.get("open")),
                    format_price(c.get("high")),
                    format_price(c.get("low")),
                    format_price(c.get("close")),
                    str(c.get("volume", "")),
                ]
            )
        print_markdown_table(["Time", "Open", "High", "Low", "Close", "Volume"], rows)
        return

    table = Table(title=f"Price History: {ticker} (last {days} days)")
    table.add_column("Time", style="dim")
    table.add_column("Open", justify="right")
    table.add_column("High", style="green", justify="right")
    table.add_column("Low", style="red", justify="right")
    table.add_column("Close", justify="right")
    table.add_column("Vol", style="yellow", justify="right")

    for c in candles[-20:]:
        ts = c.get("end_period_ts", 0)
        date_str = time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)) if ts else ""
        table.add_row(
            date_str,
            format_price(c.get("open")),
            format_price(c.get("high")),
            format_price(c.get("low")),
            format_price(c.get("close")),
            str(c.get("volume", "")),
        )

    console.print(table)


@app.command()
def events(
    status: str = typer.Option(
        "open", "--status", "-s", help="Status filter (open/closed/settled)"
    ),
    series: str = typer.Option(None, "--series", help="Filter by series ticker"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List event categories."""
    client = get_client()
    try:
        data = client.list_events(
            status=status if status != "all" else None,
            series_ticker=series,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    events_list = data.get("events", [])

    if json_output:
        print(json.dumps(events_list, indent=2))
        return

    if not events_list:
        console.print("[yellow]No events found[/]")
        return

    if markdown:
        rows = []
        for e in events_list:
            rows.append(
                [
                    e.get("event_ticker", ""),
                    e.get("title", "")[:50],
                    e.get("category", ""),
                    e.get("series_ticker", ""),
                ]
            )
        print_markdown_table(["Ticker", "Title", "Category", "Series"], rows)
        return

    table = Table(title="Kalshi Events")
    table.add_column("Ticker", style="cyan", max_width=30)
    table.add_column("Title", style="white", max_width=40)
    table.add_column("Category", style="green")
    table.add_column("Series", style="dim")

    for e in events_list:
        table.add_row(
            e.get("event_ticker", ""),
            e.get("title", "")[:40],
            e.get("category", ""),
            e.get("series_ticker", ""),
        )

    console.print(table)


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search markets by title."""
    client = get_client()
    try:
        data = client.list_markets(status="open", limit=500)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    markets_list = data.get("markets", [])
    query_lower = query.lower()
    filtered = [
        m
        for m in markets_list
        if query_lower in m.get("title", "").lower()
        or query_lower in m.get("ticker", "").lower()
        or query_lower in m.get("subtitle", "").lower()
    ][:limit]

    if json_output:
        print(json.dumps(filtered, indent=2))
        return

    if not filtered:
        console.print(f"[yellow]No markets found matching '{query}'[/]")
        return

    if markdown:
        rows = []
        for m in filtered:
            yes_price = m.get("yes_ask") or m.get("last_price")
            rows.append(
                [
                    m.get("ticker", ""),
                    m.get("title", "")[:50],
                    format_price(yes_price),
                    format_volume(m.get("volume")),
                ]
            )
        print_markdown_table(["Ticker", "Title", "Yes Price", "Volume"], rows)
        return

    table = Table(title=f"Search Results: '{query}'")
    table.add_column("Ticker", style="cyan", max_width=25)
    table.add_column("Title", style="white", max_width=40)
    table.add_column("Yes", style="green", justify="right")
    table.add_column("Volume", style="yellow", justify="right")

    for m in filtered:
        yes_price = m.get("yes_ask") or m.get("last_price")
        table.add_row(
            m.get("ticker", ""),
            m.get("title", "")[:40],
            format_price(yes_price),
            format_volume(m.get("volume")),
        )

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /markets)"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
):
    """Make a raw API call."""
    client = get_client()

    query_params = None
    if params:
        query_params = {}
        for pair in params.split(","):
            if "=" in pair:
                k, v = pair.split("=", 1)
                query_params[k.strip()] = v.strip()

    try:
        data = client._request(endpoint, params=query_params)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
