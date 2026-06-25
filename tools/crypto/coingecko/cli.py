"""CLI for CoinGecko API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="coingecko", help="CoinGecko CLI for cryptocurrency market data")


@app.command("health")
def health():
    """Assert coingecko connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_markets(per_page=1)
        payload = {"ok": True, "tool": "coingecko", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "coingecko", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import CoinGeckoClient

    return CoinGeckoClient()


def format_number(value: float | None, decimals: int = 2, prefix: str = "$") -> str:
    """Format large numbers with B/M/K suffixes."""
    if value is None:
        return "N/A"
    if value >= 1e12:
        return f"{prefix}{value / 1e12:.{decimals}f}T"
    elif value >= 1e9:
        return f"{prefix}{value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"{prefix}{value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"{prefix}{value / 1e3:.{decimals}f}K"
    return f"{prefix}{value:.{decimals}f}"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def price(
    ids: str = typer.Argument(..., help="Coin IDs (comma-separated, e.g., bitcoin,ethereum)"),
    vs_currency: str = typer.Option("usd", "--vs", "-v", help="Target currency"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get current prices for coins."""
    client = get_client()
    data = client.get_price(ids, vs_currency)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for coin_id, info in data.items():
            price_val = info.get(vs_currency, 0)
            mcap = info.get(f"{vs_currency}_market_cap", 0)
            vol = info.get(f"{vs_currency}_24h_vol", 0)
            change = info.get(f"{vs_currency}_24h_change", 0)
            rows.append(
                [
                    coin_id,
                    format_number(price_val),
                    format_number(mcap),
                    format_number(vol),
                    f"{change:+.2f}%" if change else "N/A",
                ]
            )
        print_markdown_table(["Coin", "Price", "Market Cap", "24h Volume", "24h Change"], rows)
        return

    table = Table(title="Prices")
    table.add_column("Coin", style="cyan")
    table.add_column("Price", style="yellow", justify="right")
    table.add_column("Market Cap", style="green", justify="right")
    table.add_column("24h Volume", style="blue", justify="right")
    table.add_column("24h Change", justify="right")

    for coin_id, info in data.items():
        price_val = info.get(vs_currency, 0)
        mcap = info.get(f"{vs_currency}_market_cap", 0)
        vol = info.get(f"{vs_currency}_24h_vol", 0)
        change = info.get(f"{vs_currency}_24h_change", 0)
        change_color = "green" if change and change >= 0 else "red"
        table.add_row(
            coin_id,
            format_number(price_val),
            format_number(mcap),
            format_number(vol),
            f"[{change_color}]{change:+.2f}%[/]" if change else "N/A",
        )

    console.print(table)


@app.command()
def markets(
    vs_currency: str = typer.Option("usd", "--vs", "-v", help="Target currency"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List coins by market cap."""
    client = get_client()
    data = client.get_markets(vs_currency=vs_currency, per_page=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for coin in data:
            change = coin.get("price_change_percentage_24h")
            rows.append(
                [
                    f"#{coin.get('market_cap_rank', 'N/A')}",
                    coin.get("symbol", "").upper(),
                    coin.get("name", ""),
                    format_number(coin.get("current_price")),
                    format_number(coin.get("market_cap")),
                    f"{change:+.2f}%" if change else "N/A",
                ]
            )
        print_markdown_table(["Rank", "Symbol", "Name", "Price", "Market Cap", "24h Change"], rows)
        return

    table = Table(title="Markets by Market Cap")
    table.add_column("Rank", style="dim", justify="right")
    table.add_column("Symbol", style="cyan")
    table.add_column("Name", style="white", max_width=20)
    table.add_column("Price", style="yellow", justify="right")
    table.add_column("Market Cap", style="green", justify="right")
    table.add_column("24h Change", justify="right")

    for coin in data:
        change = coin.get("price_change_percentage_24h")
        change_color = "green" if change and change >= 0 else "red"
        table.add_row(
            f"#{coin.get('market_cap_rank', 'N/A')}",
            coin.get("symbol", "").upper(),
            coin.get("name", ""),
            format_number(coin.get("current_price")),
            format_number(coin.get("market_cap")),
            f"[{change_color}]{change:+.2f}%[/]" if change else "N/A",
        )

    console.print(table)


@app.command()
def coin(
    coin_id: str = typer.Argument(..., help="Coin ID (e.g., bitcoin, ethereum)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get coin details."""
    client = get_client()
    data = client.get_coin(coin_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    market = data.get("market_data", {})
    current_price = market.get("current_price", {}).get("usd", 0)
    mcap = market.get("market_cap", {}).get("usd", 0)
    vol = market.get("total_volume", {}).get("usd", 0)
    change_24h = market.get("price_change_percentage_24h", 0)
    change_7d = market.get("price_change_percentage_7d", 0)
    change_30d = market.get("price_change_percentage_30d", 0)
    ath = market.get("ath", {}).get("usd", 0)
    ath_change = market.get("ath_change_percentage", {}).get("usd", 0)

    if markdown:
        print(f"# {data.get('name', coin_id)} ({data.get('symbol', '').upper()})\n")
        print(f"**Rank:** #{data.get('market_cap_rank', 'N/A')}")
        print(f"**Price:** {format_number(current_price)}")
        print(f"**Market Cap:** {format_number(mcap)}")
        print(f"**24h Volume:** {format_number(vol)}")
        print("\n**Price Changes:**")
        print(f"- 24h: {change_24h:+.2f}%" if change_24h else "- 24h: N/A")
        print(f"- 7d: {change_7d:+.2f}%" if change_7d else "- 7d: N/A")
        print(f"- 30d: {change_30d:+.2f}%" if change_30d else "- 30d: N/A")
        print(f"\n**ATH:** {format_number(ath)} ({ath_change:+.2f}% from ATH)")
        return

    console.print(f"\n[bold cyan]{data.get('name', coin_id)}[/] ({data.get('symbol', '').upper()})")
    console.print(f"[dim]Rank #{data.get('market_cap_rank', 'N/A')}[/]\n")

    console.print(f"Price: [yellow]{format_number(current_price)}[/]")
    console.print(f"Market Cap: [green]{format_number(mcap)}[/]")
    console.print(f"24h Volume: [blue]{format_number(vol)}[/]")

    console.print("\n[bold]Price Changes:[/]")
    for label, val in [("24h", change_24h), ("7d", change_7d), ("30d", change_30d)]:
        if val:
            color = "green" if val >= 0 else "red"
            console.print(f"  {label}: [{color}]{val:+.2f}%[/]")
        else:
            console.print(f"  {label}: N/A")

    console.print(f"\n[bold]ATH:[/] {format_number(ath)} ({ath_change:+.2f}% from ATH)")


@app.command()
def trending(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get trending coins."""
    client = get_client()
    data = client.get_trending()

    coins = data.get("coins", [])

    if json_output:
        print(json.dumps(coins, indent=2))
        return

    if markdown:
        rows = []
        for item in coins:
            c = item.get("item", {})
            rows.append(
                [
                    f"#{c.get('market_cap_rank', 'N/A')}",
                    c.get("symbol", "").upper(),
                    c.get("name", ""),
                    f"#{c.get('score', 0) + 1}",
                ]
            )
        print_markdown_table(["Rank", "Symbol", "Name", "Trending #"], rows)
        return

    table = Table(title="Trending Coins")
    table.add_column("Rank", style="dim", justify="right")
    table.add_column("Symbol", style="cyan")
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Trending #", style="yellow", justify="right")

    for item in coins:
        c = item.get("item", {})
        table.add_row(
            f"#{c.get('market_cap_rank', 'N/A')}",
            c.get("symbol", "").upper(),
            c.get("name", ""),
            f"#{c.get('score', 0) + 1}",
        )

    console.print(table)


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for coins."""
    client = get_client()
    data = client.search(query)

    coins = data.get("coins", [])

    if json_output:
        print(json.dumps(coins, indent=2))
        return

    if not coins:
        console.print(f"[yellow]No results for '{query}'[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for c in coins[:20]:
            rows.append(
                [
                    c.get("id", ""),
                    c.get("symbol", "").upper(),
                    c.get("name", ""),
                    f"#{c.get('market_cap_rank', 'N/A')}",
                ]
            )
        print_markdown_table(["ID", "Symbol", "Name", "Rank"], rows)
        return

    table = Table(title=f"Search Results: '{query}'")
    table.add_column("ID", style="dim")
    table.add_column("Symbol", style="cyan")
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Rank", style="yellow", justify="right")

    for c in coins[:20]:
        table.add_row(
            c.get("id", ""),
            c.get("symbol", "").upper(),
            c.get("name", ""),
            f"#{c.get('market_cap_rank', 'N/A')}",
        )

    console.print(table)


@app.command()
def history(
    coin_id: str = typer.Argument(..., help="Coin ID (e.g., bitcoin)"),
    days: int = typer.Option(30, "--days", "-d", help="Number of days"),
    vs_currency: str = typer.Option("usd", "--vs", "-v", help="Target currency"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get historical price data."""
    client = get_client()
    data = client.get_market_chart(coin_id, vs_currency=vs_currency, days=days)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    prices = data.get("prices", [])
    if not prices:
        console.print(f"[yellow]No price data for '{coin_id}'[/]")
        raise typer.Exit()

    first_price = prices[0][1]
    last_price = prices[-1][1]
    change = ((last_price - first_price) / first_price) * 100 if first_price else 0
    high = max(p[1] for p in prices)
    low = min(p[1] for p in prices)

    if markdown:
        print(f"# {coin_id.title()} - {days} Day History\n")
        print(f"**Start:** {format_number(first_price)}")
        print(f"**End:** {format_number(last_price)}")
        print(f"**Change:** {change:+.2f}%")
        print(f"**High:** {format_number(high)}")
        print(f"**Low:** {format_number(low)}")
        return

    color = "green" if change >= 0 else "red"
    console.print(f"\n[bold]{coin_id.title()}[/] - {days} Day History\n")
    console.print(f"Start: {format_number(first_price)}")
    console.print(f"End:   {format_number(last_price)}")
    console.print(f"[{color}]Change: {change:+.2f}%[/]")
    console.print(f"High: {format_number(high)}")
    console.print(f"Low:  {format_number(low)}")


@app.command()
def categories(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List coin categories."""
    client = get_client()
    data = client.get_categories()

    data = sorted(data, key=lambda x: x.get("market_cap") or 0, reverse=True)[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for cat in data:
            change = cat.get("market_cap_change_24h")
            rows.append(
                [
                    cat.get("name", ""),
                    format_number(cat.get("market_cap")),
                    format_number(cat.get("volume_24h")),
                    f"{change:+.2f}%" if change else "N/A",
                ]
            )
        print_markdown_table(["Category", "Market Cap", "24h Volume", "24h Change"], rows)
        return

    table = Table(title="Categories by Market Cap")
    table.add_column("Category", style="cyan", max_width=30)
    table.add_column("Market Cap", style="green", justify="right")
    table.add_column("24h Volume", style="blue", justify="right")
    table.add_column("24h Change", justify="right")

    for cat in data:
        change = cat.get("market_cap_change_24h")
        change_color = "green" if change and change >= 0 else "red"
        table.add_row(
            cat.get("name", ""),
            format_number(cat.get("market_cap")),
            format_number(cat.get("volume_24h")),
            f"[{change_color}]{change:+.2f}%[/]" if change else "N/A",
        )

    console.print(table)


@app.command()
def exchanges(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List exchanges by trading volume."""
    client = get_client()
    data = client.get_exchanges(per_page=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for ex in data:
            rows.append(
                [
                    f"#{ex.get('trust_score_rank', 'N/A')}",
                    ex.get("name", ""),
                    format_number(ex.get("trade_volume_24h_btc", 0), prefix="₿"),
                    str(ex.get("trust_score", "N/A")),
                    ex.get("country", "N/A") or "N/A",
                ]
            )
        print_markdown_table(["Rank", "Name", "24h Volume (BTC)", "Trust Score", "Country"], rows)
        return

    table = Table(title="Exchanges by Volume")
    table.add_column("Rank", style="dim", justify="right")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("24h Volume (BTC)", style="yellow", justify="right")
    table.add_column("Trust Score", style="green", justify="center")
    table.add_column("Country", style="dim", max_width=15)

    for ex in data:
        table.add_row(
            f"#{ex.get('trust_score_rank', 'N/A')}",
            ex.get("name", ""),
            format_number(ex.get("trade_volume_24h_btc", 0), prefix="₿"),
            str(ex.get("trust_score", "N/A")),
            ex.get("country", "N/A") or "N/A",
        )

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /ping, /coins/list)"),
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
