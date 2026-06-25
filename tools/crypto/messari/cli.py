"""CLI for Messari API."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from centaur_sdk import Table

from .client import MessariClient

load_dotenv()

app = typer.Typer(name="messari", help="Messari CLI for crypto asset data and analytics")


@app.command("health")
def health():
    """Assert messari connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_assets(limit=1)
        payload = {"ok": True, "tool": "messari", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "messari", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client() -> MessariClient:
    return MessariClient()


def format_number(value: float | None, decimals: int = 2, prefix: str = "$") -> str:
    """Format large numbers with B/M/K suffixes."""
    if value is None:
        return "N/A"
    if value >= 1e9:
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
def assets(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    fields: str = typer.Option(None, "--fields", "-f", help="Comma-separated fields to include"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List crypto assets."""
    if fields:
        console.print("[yellow]--fields is ignored by the current Messari metrics endpoint[/]")
    data = get_client().list_assets(limit=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for a in data:
            symbol = a.get("symbol", "")
            name = a.get("name", "")
            slug = a.get("slug", "")
            metrics = a.get("metrics", {}) or {}
            market_data = metrics.get("market_data", {}) or {}
            price = market_data.get("price_usd")
            rows.append([symbol, name, slug, format_number(price) if price else "N/A"])
        print_markdown_table(["Symbol", "Name", "Slug", "Price"], rows)
        return

    table = Table(title="Crypto Assets")
    table.add_column("Symbol", style="cyan", max_width=10)
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Slug", style="dim", max_width=20)
    table.add_column("Price", style="yellow", justify="right")

    for a in data:
        symbol = a.get("symbol", "")
        name = a.get("name", "")
        slug = a.get("slug", "")
        metrics = a.get("metrics", {}) or {}
        market_data = metrics.get("market_data", {}) or {}
        price = market_data.get("price_usd")
        table.add_row(symbol, name, slug, format_number(price) if price else "N/A")

    console.print(table)


@app.command()
def asset(
    asset_key: str = typer.Argument(..., help="Asset slug or ID (e.g., bitcoin, ethereum)"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get details for a specific asset."""
    data = get_client().get_asset(asset_key)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = data.get("name", asset_key)
    symbol = data.get("symbol", "")
    slug = data.get("slug", "")

    if markdown:
        print(f"# {name} ({symbol})\n")
        print(f"- **Slug:** {slug}")
        print(f"- **ID:** {data.get('id', 'N/A')}")
        return

    console.print(f"\n[bold cyan]{name}[/] ({symbol})")
    console.print(f"[dim]Slug: {slug} | ID: {data.get('id', 'N/A')}[/]\n")


@app.command()
def metrics(
    asset_key: str = typer.Argument(..., help="Asset slug or ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get metrics for an asset (price, volume, market cap, etc)."""
    data = get_client().get_asset_metrics(asset_key)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = data.get("name", asset_key)
    symbol = data.get("symbol", "")
    market_data = data.get("market_data", {}) or {}
    marketcap = data.get("marketcap", {}) or {}

    price = market_data.get("price_usd")
    volume_24h = market_data.get("real_volume_last_24_hours")
    mcap = marketcap.get("current_marketcap_usd")
    change_24h = market_data.get("percent_change_usd_last_24_hours")

    if markdown:
        print(f"# {name} ({symbol}) Metrics\n")
        print(f"- **Price:** {format_number(price)}")
        print(f"- **24h Volume:** {format_number(volume_24h)}")
        print(f"- **Market Cap:** {format_number(mcap)}")
        if change_24h is not None:
            print(f"- **24h Change:** {change_24h:+.2f}%")
        return

    console.print(f"\n[bold cyan]{name}[/] ({symbol}) Metrics\n")
    console.print(f"Price:      [yellow]{format_number(price)}[/]")
    console.print(f"24h Volume: [green]{format_number(volume_24h)}[/]")
    console.print(f"Market Cap: [blue]{format_number(mcap)}[/]")
    if change_24h is not None:
        color = "green" if change_24h >= 0 else "red"
        console.print(f"24h Change: [{color}]{change_24h:+.2f}%[/]")


@app.command()
def profile(
    asset_key: str = typer.Argument(..., help="Asset slug or ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get asset profile (description, links, team, etc)."""
    data = get_client().get_asset_profile(asset_key)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = data.get("name", asset_key)
    symbol = data.get("symbol", "")
    profile_data = data.get("profile", {}) or {}
    general = profile_data.get("general", {}) or {}
    overview = general.get("overview", {}) or {}

    tagline = overview.get("tagline", "")
    description = overview.get("project_details", "")[:500]

    if markdown:
        print(f"# {name} ({symbol})\n")
        if tagline:
            print(f"*{tagline}*\n")
        if description:
            print(f"{description}...")
        return

    console.print(f"\n[bold cyan]{name}[/] ({symbol})")
    if tagline:
        console.print(f"[italic]{tagline}[/]\n")
    if description:
        console.print(f"{description}...\n")


@app.command()
def markets(
    asset_key: str = typer.Argument(..., help="Asset slug or ID"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get markets for an asset."""
    data = get_client().get_asset_markets(asset_key)[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for m in data:
            exchange = m.get("exchange_name", "")
            pair = m.get("pair", "")
            price = m.get("price_usd")
            volume = m.get("volume_last_24_hours")
            rows.append([exchange, pair, format_number(price), format_number(volume)])
        print_markdown_table(["Exchange", "Pair", "Price", "24h Volume"], rows)
        return

    table = Table(title=f"Markets for {asset_key}")
    table.add_column("Exchange", style="cyan", max_width=20)
    table.add_column("Pair", style="white", max_width=15)
    table.add_column("Price", style="yellow", justify="right")
    table.add_column("24h Volume", style="green", justify="right")

    for m in data:
        exchange = m.get("exchange_name", "")
        pair = m.get("pair", "")
        price = m.get("price_usd")
        volume = m.get("volume_last_24_hours")
        table.add_row(exchange, pair, format_number(price), format_number(volume))

    console.print(table)


@app.command()
def news(
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get latest crypto news."""
    data = get_client().get_news(limit=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        for article in data:
            title = article.get("title", "")
            url = article.get("url", "")
            published = article.get("published_at", "")[:10]
            print(f"- [{title}]({url}) ({published})")
        return

    for article in data:
        title = article.get("title", "")
        url = article.get("url", "")
        published = article.get("published_at", "")[:10]
        author = article.get("author", {}) or {}
        author_name = author.get("name", "Unknown")
        console.print(f"[cyan]{title}[/]")
        console.print(f"  [dim]{published} | {author_name}[/]")
        console.print(f"  [blue]{url}[/]\n")


@app.command()
def timeseries(
    asset_key: str = typer.Argument(..., help="Asset slug or ID"),
    metric: str = typer.Argument(..., help="Metric key (e.g., price, mcap, vol)"),
    start: str = typer.Option(None, "--start", "-s", help="Start date (YYYY-MM-DD)"),
    end: str = typer.Option(None, "--end", "-e", help="End date (YYYY-MM-DD)"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get historical timeseries data for an asset metric."""
    data = get_client().get_timeseries(asset_key, metric, start=start, end=end)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    values = data.get("values", [])
    if not values:
        console.print("[yellow]No timeseries data found.[/]")
        raise typer.Exit()

    if markdown:
        print("| Timestamp | Value |")
        print("|---|---|")
        for row in values[-20:]:
            if len(row) >= 2:
                print(f"| {row[0]} | {row[1]} |")
        return

    table = Table(title=f"{asset_key} - {metric}")
    table.add_column("Timestamp", style="dim")
    table.add_column("Value", style="yellow", justify="right")

    for row in values[-20:]:
        if len(row) >= 2:
            table.add_row(str(row[0]), str(row[1]))

    console.print(table)
    if len(values) > 20:
        console.print(f"[dim]Showing last 20 of {len(values)} data points[/]")


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /assets)"),
    version: int = typer.Option(1, "--version", "-v", help="API version (1 or 2)"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
):
    """Make a raw API call.

    Examples:
        messari raw /assets --params limit=5
        messari raw /assets/bitcoin/profile --version 2
    """
    query_params = None
    if params:
        query_params = {}
        for pair in params.split(","):
            if "=" in pair:
                k, v = pair.split("=", 1)
                query_params[k.strip()] = v.strip()

    try:
        data = get_client().raw_request(endpoint, version=version, params=query_params)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1) from e


if __name__ == "__main__":
    app()
