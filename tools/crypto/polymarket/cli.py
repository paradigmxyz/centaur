"""CLI for Polymarket prediction markets."""

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="polymarket", help="Polymarket CLI for prediction market data")


@app.command("health")
def health():
    """Assert polymarket connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_markets(limit=1)
        payload = {"ok": True, "tool": "polymarket", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "polymarket", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import PolymarketClient

    return PolymarketClient()


def format_number(value: float, decimals: int = 2) -> str:
    """Format large numbers with B/M/K suffixes."""
    if value >= 1e9:
        return f"${value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"${value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"${value / 1e3:.{decimals}f}K"
    return f"${value:.{decimals}f}"


def format_percent(value: float | None) -> str:
    """Format as percentage."""
    if value is None:
        return "-"
    return f"{value * 100:.1f}%"


def parse_prices(prices_str: str | None) -> list[float]:
    """Parse outcome prices from JSON string."""
    if not prices_str:
        return []
    try:
        return [float(p) for p in json.loads(prices_str)]
    except (json.JSONDecodeError, ValueError):
        return []


def parse_outcomes(outcomes_str: str | None) -> list[str]:
    """Parse outcomes from JSON string."""
    if not outcomes_str:
        return []
    try:
        return json.loads(outcomes_str)
    except json.JSONDecodeError:
        return []


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def markets(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results to show"),
    closed: bool = typer.Option(False, "--closed", help="Include closed markets"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List active prediction markets sorted by volume."""
    client = get_client()
    data = client.list_markets(limit=limit, closed=closed)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for m in data:
            prices = parse_prices(m.get("outcomePrices"))
            price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
            volume = m.get("volumeNum") or 0
            rows.append(
                [
                    m.get("question", "")[:60],
                    price_str,
                    format_number(volume),
                    str(m.get("id", "")),
                ]
            )
        print_markdown_table(["Question", "Yes Price", "Volume", "ID"], rows)
        return

    table = Table(title="Active Markets")
    table.add_column("Question", style="cyan", max_width=50)
    table.add_column("Yes", style="green", justify="right")
    table.add_column("Volume", style="yellow", justify="right")
    table.add_column("ID", style="dim")

    for m in data:
        prices = parse_prices(m.get("outcomePrices"))
        price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
        volume = m.get("volumeNum") or 0
        table.add_row(
            (m.get("question", "") or "")[:50],
            price_str,
            format_number(volume),
            str(m.get("id", "")),
        )

    console.print(table)


@app.command()
def market(
    market_id: str = typer.Argument(..., help="Market ID or slug"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details for a specific market."""
    client = get_client()
    data = client.get_market(market_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('question', 'Unknown')}[/]")
    console.print(f"[dim]ID: {data.get('id')} | Slug: {data.get('slug', 'N/A')}[/]\n")

    outcomes = parse_outcomes(data.get("outcomes"))
    prices = parse_prices(data.get("outcomePrices"))

    if outcomes and prices:
        console.print("[bold]Prices:[/]")
        for outcome, price in zip(outcomes, prices):
            color = "green" if price > 0.5 else "yellow"
            console.print(f"  [{color}]{outcome}: {price * 100:.1f}%[/]")
        console.print()

    volume = data.get("volumeNum") or data.get("volume") or 0
    liquidity = data.get("liquidityNum") or data.get("liquidity") or 0
    console.print(f"Volume: [yellow]{format_number(float(volume))}[/]")
    console.print(f"Liquidity: [green]{format_number(float(liquidity))}[/]")

    if data.get("volume24hr"):
        console.print(f"24h Volume: {format_number(data['volume24hr'])}")

    console.print(f"Status: {'[red]Closed[/]' if data.get('closed') else '[green]Active[/]'}")

    if data.get("endDate"):
        console.print(f"End Date: {data['endDate']}")

    if data.get("description"):
        console.print(f"\n[dim]{data['description'][:200]}...[/]")


@app.command()
def trades(
    market_id: str = typer.Argument(..., help="Market condition ID"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max trades to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get recent trades for a market (requires market condition ID)."""
    client = get_client()

    try:
        data = client.get_trades(market=market_id)
    except RuntimeError as e:
        if "401" in str(e) or "403" in str(e):
            console.print("[yellow]Note: Trade data requires authentication.[/]")
            console.print("[dim]Use market's conditionId for the market parameter.[/]")
            raise typer.Exit(1)
        raise

    if json_output:
        print(json.dumps(data[:limit] if isinstance(data, list) else data, indent=2))
        return

    if not data:
        console.print("[yellow]No trades found for this market.[/]")
        return

    trades_list = data[:limit] if isinstance(data, list) else []

    table = Table(title=f"Recent Trades: {market_id[:20]}...")
    table.add_column("Side", style="cyan")
    table.add_column("Size", justify="right")
    table.add_column("Price", justify="right")
    table.add_column("Time", style="dim")

    for t in trades_list:
        side_color = "green" if t.get("side") == "buy" else "red"
        table.add_row(
            f"[{side_color}]{t.get('side', '')}[/]",
            t.get("size", ""),
            t.get("price", ""),
            t.get("match_time", "")[:19] if t.get("match_time") else "",
        )

    console.print(table)


@app.command()
def history(
    token_id: str = typer.Argument(..., help="CLOB token ID"),
    interval: str = typer.Option("1w", "--interval", "-i", help="1m, 1w, 1d, 6h, 1h, max"),
    fidelity: int = typer.Option(None, "--fidelity", "-f", help="Resolution in minutes"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get price/volume history for a market token."""
    client = get_client()
    data = client.get_price_history(token_id, interval=interval, fidelity=fidelity)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    history_data = data.get("history", [])
    if not history_data:
        console.print("[yellow]No price history available.[/]")
        return

    console.print(f"\n[bold]Price History[/] (interval: {interval})\n")

    recent = history_data[-10:]
    table = Table()
    table.add_column("Time", style="dim")
    table.add_column("Price", style="green", justify="right")

    for point in recent:
        from datetime import datetime

        ts = point.get("t", 0)
        price = point.get("p", 0)
        time_str = datetime.fromtimestamp(ts).strftime("%Y-%m-%d %H:%M") if ts else "-"
        table.add_row(time_str, f"{price * 100:.1f}%")

    console.print(table)

    if len(history_data) > 1:
        first_price = history_data[0].get("p", 0)
        last_price = history_data[-1].get("p", 0)
        change = (last_price - first_price) * 100
        color = "green" if change >= 0 else "red"
        console.print(f"\n[{color}]Change: {change:+.1f}pp[/]")


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results per type"),
    closed: bool = typer.Option(False, "--closed", help="Include closed markets"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Search markets by keyword."""
    client = get_client()
    data = client.search(query, limit=limit, closed=closed)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    events = data.get("events") or []

    if not events:
        console.print(f"[yellow]No results for '{query}'[/]")
        return

    if markdown:
        rows = []
        for event in events[:limit]:
            markets_list = event.get("markets", [])
            for m in markets_list[:1]:
                prices = parse_prices(m.get("outcomePrices"))
                price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
                rows.append(
                    [
                        event.get("title", "")[:50],
                        price_str,
                        str(event.get("id", "")),
                    ]
                )
        print_markdown_table(["Event", "Yes Price", "ID"], rows)
        return

    table = Table(title=f"Search: '{query}'")
    table.add_column("Event", style="cyan", max_width=50)
    table.add_column("Yes", style="green", justify="right")
    table.add_column("ID", style="dim")

    for event in events[:limit]:
        markets_list = event.get("markets", [])
        for m in markets_list[:1]:
            prices = parse_prices(m.get("outcomePrices"))
            price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
            table.add_row(
                (event.get("title", "") or "")[:50],
                price_str,
                str(event.get("id", "")),
            )

    console.print(table)


@app.command()
def trending(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Show trending/hot markets by 24h volume."""
    client = get_client()
    data = client.list_markets(limit=limit, closed=False, order="volume24hr", ascending=False)
    data = sorted(data, key=lambda x: x.get("volume24hr") or 0, reverse=True)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for m in data:
            prices = parse_prices(m.get("outcomePrices"))
            price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
            vol_24h = m.get("volume24hr") or 0
            rows.append(
                [
                    m.get("question", "")[:50],
                    price_str,
                    format_number(vol_24h),
                    str(m.get("id", "")),
                ]
            )
        print_markdown_table(["Question", "Yes", "24h Volume", "ID"], rows)
        return

    table = Table(title="Trending Markets (24h Volume)")
    table.add_column("Question", style="cyan", max_width=45)
    table.add_column("Yes", style="green", justify="right")
    table.add_column("24h Vol", style="yellow", justify="right")
    table.add_column("ID", style="dim")

    for m in data:
        prices = parse_prices(m.get("outcomePrices"))
        price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
        vol_24h = m.get("volume24hr") or 0
        table.add_row(
            (m.get("question", "") or "")[:45],
            price_str,
            format_number(vol_24h),
            str(m.get("id", "")),
        )

    console.print(table)


@app.command()
def events(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    closed: bool = typer.Option(False, "--closed", help="Include closed events"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List events (groups of related markets)."""
    client = get_client()
    data = client.list_events(limit=limit, closed=closed)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    table = Table(title="Events")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Markets", style="green", justify="right")
    table.add_column("ID", style="dim")

    for e in data:
        markets_count = len(e.get("markets", []))
        table.add_row(
            (e.get("title", "") or "")[:50],
            str(markets_count),
            str(e.get("id", "")),
        )

    console.print(table)


@app.command()
def event(
    event_id: str = typer.Argument(..., help="Event ID or slug"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details for a specific event."""
    client = get_client()
    data = client.get_event(event_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('title', 'Unknown')}[/]")
    console.print(f"[dim]ID: {data.get('id')} | Slug: {data.get('slug', 'N/A')}[/]\n")

    markets_list = data.get("markets", [])
    if markets_list:
        console.print(f"[bold]Markets ({len(markets_list)}):[/]")
        for m in markets_list[:10]:
            prices = parse_prices(m.get("outcomePrices"))
            price_str = f"{prices[0] * 100:.0f}%" if prices else "-"
            question = (m.get("question") or m.get("groupItemTitle") or "")[:60]
            console.print(f"  • {question} [green]{price_str}[/]")

        if len(markets_list) > 10:
            console.print(f"  ... and {len(markets_list) - 10} more")


@app.command()
def price(
    token_id: str = typer.Argument(..., help="CLOB token ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get current price for a token."""
    client = get_client()
    data = client.get_price(token_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    price_val = data.get("price", 0)
    console.print(f"Price: [green]{float(price_val) * 100:.2f}%[/]")


@app.command()
def book(
    token_id: str = typer.Argument(..., help="CLOB token ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get orderbook for a token."""
    client = get_client()
    data = client.get_book(token_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print("\n[bold]Orderbook[/]\n")

    bids = data.get("bids", [])[:5]
    asks = data.get("asks", [])[:5]

    if asks:
        console.print("[red]Asks:[/]")
        for ask in reversed(asks):
            console.print(f"  {float(ask.get('price', 0)) * 100:.2f}% - {ask.get('size', 0)}")

    console.print("---")

    if bids:
        console.print("[green]Bids:[/]")
        for bid in bids:
            console.print(f"  {float(bid.get('price', 0)) * 100:.2f}% - {bid.get('size', 0)}")


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint path"),
    api: str = typer.Option("gamma", "--api", "-a", help="API: gamma or clob"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
):
    """Make a raw API call."""
    client = get_client()

    base_url = client.gamma_url if api == "gamma" else client.clob_url

    query_params = None
    if params:
        query_params = {}
        for pair in params.split(","):
            if "=" in pair:
                k, v = pair.split("=", 1)
                query_params[k.strip()] = v.strip()

    try:
        data = client._request(f"{base_url}{endpoint}", params=query_params)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
