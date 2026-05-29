"""CLI for SimilarWeb API."""

import json
from datetime import date, timedelta

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="similarweb", help="SimilarWeb CLI for web traffic and market intelligence")
console = Console()


def get_client():
    from .client import SimilarWebClient

    return SimilarWebClient()


def format_number(value: float, decimals: int = 2) -> str:
    """Format large numbers with B/M/K suffixes."""
    if value >= 1e9:
        return f"{value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"{value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"{value / 1e3:.{decimals}f}K"
    return f"{value:.{decimals}f}"


def format_percent(value: float) -> str:
    """Format as percentage."""
    return f"{value * 100:.1f}%" if value < 1 else f"{value:.1f}%"


def format_duration(seconds: float) -> str:
    """Format seconds as mm:ss."""
    mins = int(seconds // 60)
    secs = int(seconds % 60)
    return f"{mins}:{secs:02d}"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def parse_date(date_str: str) -> date:
    """Parse date string in YYYY-MM-DD or YYYY-MM format."""
    if len(date_str) == 7:
        date_str += "-01"
    from datetime import datetime

    return datetime.strptime(date_str, "%Y-%m-%d").date()


def default_dates() -> tuple[date, date]:
    """Return default date range (last 3 months)."""
    end = date.today().replace(day=1) - timedelta(days=1)
    start = (end - timedelta(days=90)).replace(day=1)
    return start, end


@app.command()
def traffic(
    domain: str = typer.Argument(..., help="Domain to analyze (e.g., google.com)"),
    start: str = typer.Option(
        None, "--start", "-s", help="Start date (YYYY-MM). Default: 3 months ago"
    ),
    end: str = typer.Option(None, "--end", "-e", help="End date (YYYY-MM). Default: last month"),
    country: str = typer.Option("world", "--country", "-c", help="Country code or 'world'"),
    granularity: str = typer.Option("monthly", "--granularity", "-g", help="daily/weekly/monthly"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get website traffic and engagement metrics."""
    client = get_client()

    start_date, end_date = default_dates()
    if start:
        start_date = parse_date(start)
    if end:
        end_date = parse_date(end)

    try:
        data = client.get_visits(domain, start_date, end_date, country, granularity)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    visits = data.get("visits", [])
    if not visits:
        console.print("[yellow]No traffic data found[/]")
        return

    if markdown:
        rows = [[v.get("date", ""), format_number(v.get("visits", 0))] for v in visits[-12:]]
        print_markdown_table(["Date", "Visits"], rows)
        return

    console.print(f"\n[bold cyan]Traffic: {domain}[/] ({country})\n")

    table = Table(title="Monthly Visits")
    table.add_column("Date", style="dim")
    table.add_column("Visits", style="yellow", justify="right")

    for v in visits[-12:]:
        table.add_row(v.get("date", ""), format_number(v.get("visits", 0)))

    console.print(table)


@app.command()
def rank(
    domain: str = typer.Argument(..., help="Domain to analyze"),
    country: str = typer.Option(None, "--country", "-c", help="Country code for country rank"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get website rankings (global, country, industry)."""
    client = get_client()

    try:
        global_rank = client.get_global_rank(domain)
        industry_rank = client.get_industry_rank(domain)
        country_rank = None
        if country:
            country_rank = client.get_country_rank(domain, country)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    result = {
        "domain": domain,
        "global_rank": global_rank,
        "industry_rank": industry_rank,
    }
    if country_rank:
        result["country_rank"] = country_rank

    if json_output:
        print(json.dumps(result, indent=2))
        return

    console.print(f"\n[bold cyan]Rankings: {domain}[/]\n")

    g_rank = global_rank.get("global_rank", global_rank.get("rank", "N/A"))
    console.print(
        f"[bold]Global Rank:[/] #{g_rank:,}"
        if isinstance(g_rank, int)
        else f"[bold]Global Rank:[/] {g_rank}"
    )

    i_data = industry_rank.get("category", "") or industry_rank.get("category_rank", {})
    if isinstance(i_data, dict):
        cat = i_data.get("category", "")
        i_rank = i_data.get("rank", "N/A")
    else:
        cat = i_data
        i_rank = industry_rank.get("rank", "N/A")
    console.print(
        f"[bold]Industry:[/] {cat} (#{i_rank})" if cat else f"[bold]Industry Rank:[/] #{i_rank}"
    )

    if country_rank:
        c_rank = country_rank.get("country_rank", country_rank.get("rank", "N/A"))
        console.print(
            f"[bold]Country Rank ({country.upper()}):[/] #{c_rank:,}"
            if isinstance(c_rank, int)
            else f"[bold]Country Rank:[/] {c_rank}"
        )


@app.command()
def sources(
    domain: str = typer.Argument(..., help="Domain to analyze"),
    start: str = typer.Option(None, "--start", "-s", help="Start date (YYYY-MM)"),
    end: str = typer.Option(None, "--end", "-e", help="End date (YYYY-MM)"),
    country: str = typer.Option("world", "--country", "-c", help="Country code"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get traffic sources breakdown by channel."""
    client = get_client()

    start_date, end_date = default_dates()
    if start:
        start_date = parse_date(start)
    if end:
        end_date = parse_date(end)

    try:
        data = client.get_traffic_sources(domain, start_date, end_date, country)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    overview = data.get("overview", data)
    sources_list = []
    for key in [
        "organic_search",
        "paid_search",
        "direct",
        "referrals",
        "social",
        "mail",
        "display_ads",
    ]:
        if key in overview:
            sources_list.append((key.replace("_", " ").title(), overview[key]))

    if not sources_list:
        console.print("[yellow]No traffic source data found[/]")
        return

    if markdown:
        rows = [[src, format_percent(val)] for src, val in sources_list]
        print_markdown_table(["Source", "Share"], rows)
        return

    console.print(f"\n[bold cyan]Traffic Sources: {domain}[/]\n")

    table = Table(title="Channel Breakdown")
    table.add_column("Source", style="cyan")
    table.add_column("Share", style="yellow", justify="right")

    for src, val in sorted(sources_list, key=lambda x: -x[1]):
        table.add_row(src, format_percent(val))

    console.print(table)


@app.command()
def geo(
    domain: str = typer.Argument(..., help="Domain to analyze"),
    start: str = typer.Option(None, "--start", "-s", help="Start date (YYYY-MM)"),
    end: str = typer.Option(None, "--end", "-e", help="End date (YYYY-MM)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Number of countries to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get traffic geography distribution."""
    client = get_client()

    start_date, end_date = default_dates()
    if start:
        start_date = parse_date(start)
    if end:
        end_date = parse_date(end)

    try:
        data = client.get_geography(domain, start_date, end_date)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    records = data.get("records", data.get("countries", []))
    if not records:
        console.print("[yellow]No geography data found[/]")
        return

    records = sorted(records, key=lambda x: -x.get("share", x.get("traffic_share", 0)))[:limit]

    if markdown:
        rows = []
        for r in records:
            country = r.get("country", r.get("country_name", ""))
            share = r.get("share", r.get("traffic_share", 0))
            rows.append([country, format_percent(share)])
        print_markdown_table(["Country", "Share"], rows)
        return

    console.print(f"\n[bold cyan]Geography: {domain}[/]\n")

    table = Table(title="Top Countries")
    table.add_column("Country", style="cyan")
    table.add_column("Share", style="yellow", justify="right")

    for r in records:
        country = r.get("country", r.get("country_name", ""))
        share = r.get("share", r.get("traffic_share", 0))
        table.add_row(country, format_percent(share))

    console.print(table)


@app.command()
def similar(
    domain: str = typer.Argument(..., help="Domain to find competitors for"),
    limit: int = typer.Option(20, "--limit", "-n", help="Number of results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get similar/competitor websites."""
    client = get_client()

    try:
        data = client.get_similar_sites(domain)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    sites = data.get("similar_sites", data.get("sites", []))[:limit]
    if not sites:
        console.print("[yellow]No similar sites found[/]")
        return

    if markdown:
        rows = []
        for s in sites:
            site = s.get("url", s.get("site", s)) if isinstance(s, dict) else s
            score = s.get("score", s.get("similarity", "")) if isinstance(s, dict) else ""
            rows.append([site, str(score)])
        print_markdown_table(["Site", "Score"], rows)
        return

    console.print(f"\n[bold cyan]Similar Sites: {domain}[/]\n")

    for s in sites:
        if isinstance(s, dict):
            site = s.get("url", s.get("site", ""))
            score = s.get("score", s.get("similarity", ""))
            console.print(
                f"  • {site}" + (f" ({score:.2f})" if isinstance(score, (int, float)) else "")
            )
        else:
            console.print(f"  • {s}")


@app.command()
def keywords(
    domain: str = typer.Argument(..., help="Domain to analyze"),
    start: str = typer.Option(None, "--start", "-s", help="Start date (YYYY-MM)"),
    end: str = typer.Option(None, "--end", "-e", help="End date (YYYY-MM)"),
    country: str = typer.Option("world", "--country", "-c", help="Country code"),
    limit: int = typer.Option(50, "--limit", "-n", help="Number of keywords"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get search keywords driving traffic."""
    client = get_client()

    start_date, end_date = default_dates()
    if start:
        start_date = parse_date(start)
    if end:
        end_date = parse_date(end)

    try:
        data = client.get_keywords(domain, start_date, end_date, country, limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    kws = data.get("organic", []) + data.get("paid", [])
    if not kws:
        kws = data.get("keywords", data.get("search_keywords", []))

    if not kws:
        console.print("[yellow]No keyword data found[/]")
        return

    if markdown:
        rows = []
        for k in kws[:limit]:
            kw = k.get("keyword", k.get("search_term", ""))
            share = k.get("share", k.get("traffic_share", 0))
            rows.append([kw, format_percent(share)])
        print_markdown_table(["Keyword", "Share"], rows)
        return

    console.print(f"\n[bold cyan]Keywords: {domain}[/]\n")

    table = Table(title="Top Keywords")
    table.add_column("Keyword", style="cyan", max_width=40)
    table.add_column("Share", style="yellow", justify="right")

    for k in kws[:limit]:
        kw = k.get("keyword", k.get("search_term", ""))
        share = k.get("share", k.get("traffic_share", 0))
        table.add_row(kw, format_percent(share))

    console.print(table)


@app.command("app")
def app_info(
    app_id: str = typer.Argument(..., help="App ID (package name or numeric ID)"),
    store: str = typer.Option("google", "--store", "-s", help="App store: google or apple"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get mobile app details."""
    client = get_client()

    try:
        data = client.get_app_details(app_id, store)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = data.get("title", data.get("name", app_id))
    publisher = data.get("publisher", data.get("developer", ""))
    category = data.get("category", "")
    rating = data.get("rating", "")

    console.print(f"\n[bold cyan]{name}[/]")
    console.print(f"[dim]ID: {app_id} | Store: {store}[/]\n")
    if publisher:
        console.print(f"[bold]Publisher:[/] {publisher}")
    if category:
        console.print(f"[bold]Category:[/] {category}")
    if rating:
        console.print(f"[bold]Rating:[/] {rating}")


@app.command("app-rank")
def app_ranking(
    app_id: str = typer.Argument(..., help="App ID"),
    store: str = typer.Option("google", "--store", "-s", help="App store: google or apple"),
    country: str = typer.Option("us", "--country", "-c", help="Country code"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get mobile app store ranking."""
    client = get_client()

    try:
        data = client.get_app_rank(app_id, store, country)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]App Rank: {app_id}[/] ({store}, {country.upper()})\n")
    console.print(json.dumps(data, indent=2))


@app.command()
def raw(
    endpoint: str = typer.Argument(
        ..., help="API endpoint (e.g., /v1/website/google.com/global-rank/global-rank)"
    ),
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
