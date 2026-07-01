"""CLI for SensorTower API."""

import json
from datetime import date, datetime, timedelta

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="sensortower", help="SensorTower CLI for mobile app analytics")


@app.command("health")
def health():
    """Assert sensortower connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search_apps("OpenAI", platform="ios", limit=1)
        payload = {"ok": True, "tool": "sensortower", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "sensortower", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import SensorTowerClient

    return SensorTowerClient()


def format_number(value: float, decimals: int = 2) -> str:
    """Format large numbers with B/M/K suffixes."""
    if value >= 1e9:
        return f"{value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"{value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"{value / 1e3:.{decimals}f}K"
    return f"{value:.{decimals}f}"


def format_currency(value: float, decimals: int = 2) -> str:
    """Format currency with $ prefix and B/M/K suffixes."""
    return f"${format_number(value, decimals)}"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def parse_date(date_str: str) -> date:
    """Parse date string in YYYY-MM-DD format."""
    return datetime.strptime(date_str, "%Y-%m-%d").date()


@app.command()
def downloads(
    app_id: str = typer.Argument(..., help="App ID (iOS numeric ID or Android package name)"),
    platform: str = typer.Option("ios", "--platform", "-p", help="Platform: ios or android"),
    start: str = typer.Option(
        None,
        "--start",
        "-s",
        help="Start date (YYYY-MM-DD). Default: 30 days ago",
    ),
    end: str = typer.Option(
        None,
        "--end",
        "-e",
        help="End date (YYYY-MM-DD). Default: today",
    ),
    country: str = typer.Option(None, "--country", "-c", help="Country code (e.g., US)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get app download estimates."""
    client = get_client()

    end_date = parse_date(end) if end else date.today()
    start_date = parse_date(start) if start else end_date - timedelta(days=30)
    countries = [country] if country else None

    try:
        data = client.get_sales_estimates(
            app_ids=[app_id],
            platform=platform,
            start_date=start_date,
            end_date=end_date,
            countries=countries,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No download data found[/]")
        return

    total_downloads = 0
    by_date: dict[str, int] = {}

    records = data if isinstance(data, list) else [data]
    for record in records:
        dl = record.get("iu", 0) or record.get("units", 0) or record.get("downloads", 0) or 0
        total_downloads += dl
        d = (
            record.get("d", record.get("date", ""))[:10]
            if record.get("d") or record.get("date")
            else ""
        )
        if d:
            by_date[d] = by_date.get(d, 0) + dl

    entries = sorted(by_date.items())

    if markdown:
        rows = [[d, format_number(dl)] for d, dl in entries[-10:]]
        print_markdown_table(["Date", "Downloads"], rows)
        print(f"\n**Total Downloads**: {format_number(total_downloads)}")
        return

    console.print(f"\n[bold cyan]Download Estimates: {app_id}[/] ({platform})\n")
    console.print(f"Period: {start_date} to {end_date}")
    console.print(f"[bold green]Total Downloads: {format_number(total_downloads)}[/]\n")

    if entries:
        table = Table(title="Recent Downloads")
        table.add_column("Date", style="dim")
        table.add_column("Downloads", style="yellow", justify="right")
        for d, dl in entries[-10:]:
            table.add_row(d, format_number(dl))
        console.print(table)


@app.command()
def revenue(
    app_id: str = typer.Argument(..., help="App ID (iOS numeric ID or Android package name)"),
    platform: str = typer.Option("ios", "--platform", "-p", help="Platform: ios or android"),
    start: str = typer.Option(
        None,
        "--start",
        "-s",
        help="Start date (YYYY-MM-DD). Default: 30 days ago",
    ),
    end: str = typer.Option(
        None,
        "--end",
        "-e",
        help="End date (YYYY-MM-DD). Default: today",
    ),
    country: str = typer.Option(None, "--country", "-c", help="Country code (e.g., US)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get app revenue estimates."""
    client = get_client()

    end_date = parse_date(end) if end else date.today()
    start_date = parse_date(start) if start else end_date - timedelta(days=30)
    countries = [country] if country else None

    try:
        data = client.get_sales_estimates(
            app_ids=[app_id],
            platform=platform,
            start_date=start_date,
            end_date=end_date,
            countries=countries,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No revenue data found[/]")
        return

    total_revenue = 0
    by_date: dict[str, int] = {}

    records = data if isinstance(data, list) else [data]
    for record in records:
        rev = record.get("ir", 0) or record.get("revenue", 0) or 0
        total_revenue += rev
        d = (
            record.get("d", record.get("date", ""))[:10]
            if record.get("d") or record.get("date")
            else ""
        )
        if d:
            by_date[d] = by_date.get(d, 0) + rev

    entries = sorted(by_date.items())

    if markdown:
        rows = [[d, format_currency(rev)] for d, rev in entries[-10:]]
        print_markdown_table(["Date", "Revenue"], rows)
        print(f"\n**Total Revenue**: {format_currency(total_revenue)}")
        return

    console.print(f"\n[bold cyan]Revenue Estimates: {app_id}[/] ({platform})\n")
    console.print(f"Period: {start_date} to {end_date}")
    console.print(f"[bold green]Total Revenue: {format_currency(total_revenue)}[/]\n")

    if entries:
        table = Table(title="Recent Revenue")
        table.add_column("Date", style="dim")
        table.add_column("Revenue", style="yellow", justify="right")
        for d, rev in entries[-10:]:
            table.add_row(d, format_currency(rev))
        console.print(table)


@app.command("top-charts")
def top_charts(
    platform: str = typer.Option("ios", "--platform", "-p", help="Platform: ios or android"),
    category: str = typer.Option(
        None, "--category", "-c", help="Category ID (e.g., 6014 for iOS Games)"
    ),
    country: str = typer.Option("US", "--country", help="Country code"),
    chart_type: str = typer.Option(
        "free", "--type", "-t", help="Chart type: free, paid, or grossing"
    ),
    limit: int = typer.Option(20, "--limit", "-n", help="Number of results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get top charts rankings."""
    client = get_client()

    try:
        data = client.get_top_charts(
            platform=platform,
            category=category,
            country=country,
            chart_type=chart_type,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No chart data found[/]")
        return

    apps = data if isinstance(data, list) else data.get("apps", [])

    if markdown:
        rows = []
        for i, app in enumerate(apps[:limit], 1):
            name = app.get("name", app.get("app_name", "Unknown"))
            publisher = app.get("publisher", {}).get("name", app.get("publisher_name", ""))
            rows.append([str(i), name, publisher])
        print_markdown_table(["Rank", "App", "Publisher"], rows)
        return

    title = f"Top {chart_type.title()} Apps"
    if category:
        title += f" (Category: {category})"
    title += f" - {country}"

    table = Table(title=title)
    table.add_column("#", style="dim", width=4)
    table.add_column("App", style="cyan", max_width=30)
    table.add_column("Publisher", style="green", max_width=25)
    table.add_column("Rating", style="yellow", justify="right")

    for i, app in enumerate(apps[:limit], 1):
        name = app.get("name", app.get("app_name", "Unknown"))
        publisher = app.get("publisher", {}).get("name", app.get("publisher_name", ""))
        rating = app.get("rating", app.get("average_rating", ""))
        rating_str = f"{rating:.1f}" if isinstance(rating, (int, float)) else str(rating)
        table.add_row(str(i), name, publisher, rating_str)

    console.print(table)


@app.command()
def publisher(
    publisher_id: str = typer.Argument(..., help="Publisher ID"),
    platform: str = typer.Option("ios", "--platform", "-p", help="Platform: ios or android"),
    apps: bool = typer.Option(False, "--apps", "-a", help="Also list publisher's apps"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get publisher information."""
    client = get_client()

    try:
        data = client.get_publisher(publisher_id, platform)
        if apps:
            apps_data = client.get_publisher_apps(publisher_id, platform)
            data["apps"] = apps_data
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = data.get("name", data.get("publisher_name", publisher_id))
    console.print(f"\n[bold cyan]{name}[/]")
    console.print(f"[dim]ID: {publisher_id} | Platform: {platform}[/]\n")

    if data.get("apps"):
        console.print("[bold]Apps:[/]")
        for app in data["apps"][:20]:
            app_name = app.get("name", app.get("app_name", "Unknown"))
            app_id = app.get("app_id", app.get("id", ""))
            console.print(f"  • {app_name} ({app_id})")


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    platform: str = typer.Option("ios", "--platform", "-p", help="Platform: ios or android"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for apps by name."""
    client = get_client()

    try:
        data = client.search_apps(query, platform, limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    apps = data if isinstance(data, list) else data.get("apps", [])

    if not apps:
        console.print(f"[yellow]No apps found for '{query}'[/]")
        return

    if markdown:
        rows = []
        for app in apps[:limit]:
            name = app.get("name", app.get("app_name", "Unknown"))
            app_id = app.get("app_id", app.get("id", ""))
            publisher = app.get("publisher", {}).get("name", app.get("publisher_name", ""))
            rows.append([name, app_id, publisher])
        print_markdown_table(["App", "ID", "Publisher"], rows)
        return

    table = Table(title=f"Search Results: '{query}'")
    table.add_column("App", style="cyan", max_width=30)
    table.add_column("ID", style="dim")
    table.add_column("Publisher", style="green", max_width=25)
    table.add_column("Rating", style="yellow", justify="right")

    for app in apps[:limit]:
        name = app.get("name", app.get("app_name", "Unknown"))
        app_id = app.get("app_id", app.get("id", ""))
        publisher = app.get("publisher", {}).get("name", app.get("publisher_name", ""))
        rating = app.get("rating", app.get("average_rating", ""))
        rating_str = f"{rating:.1f}" if isinstance(rating, (int, float)) else str(rating)
        table.add_row(name, str(app_id), publisher, rating_str)

    console.print(table)


@app.command("app-lookup")
def app_lookup(
    query: str = typer.Argument(..., help="App name to search for"),
    platform: str = typer.Option("ios", "--platform", "-p", help="Platform: ios or android"),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    country: str = typer.Option("US", "--country", "-c", help="Country code for store"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Look up app IDs from App Store / Play Store."""
    import httpx

    if platform.lower() in ("ios", "apple", "itunes"):
        url = "https://itunes.apple.com/search"
        params = {
            "term": query,
            "entity": "software",
            "limit": limit,
            "country": country,
        }
        try:
            resp = httpx.get(url, params=params, timeout=10)
            resp.raise_for_status()
            data = resp.json()
        except Exception as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)

        results = data.get("results", [])
        if json_output:
            print(json.dumps(results, indent=2))
            return

        if not results:
            console.print(f"[yellow]No apps found for '{query}'[/]")
            return

        if markdown:
            rows = []
            for app in results:
                rows.append(
                    [
                        app.get("trackName", ""),
                        str(app.get("trackId", "")),
                        app.get("sellerName", ""),
                    ]
                )
            print_markdown_table(["App", "App ID", "Publisher"], rows)
            return

        table = Table(title=f"iOS Apps: '{query}'")
        table.add_column("App", style="cyan", max_width=30)
        table.add_column("App ID", style="yellow")
        table.add_column("Publisher", style="green", max_width=25)
        table.add_column("Price", style="dim", justify="right")

        for app in results:
            price = app.get("formattedPrice", "Free")
            table.add_row(
                app.get("trackName", "")[:30],
                str(app.get("trackId", "")),
                app.get("sellerName", "")[:25],
                price,
            )
        console.print(table)

    else:
        console.print("[yellow]Android lookup uses Google Play scraping.[/]")
        console.print("For now, search on play.google.com and copy the package name from the URL.")
        console.print(
            "Example: play.google.com/store/apps/details?id=[bold]com.coinbase.android[/]"
        )


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /v1/ios/app/123456)"),
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
