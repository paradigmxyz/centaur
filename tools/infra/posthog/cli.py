"""CLI for PostHog API."""

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="posthog", help="PostHog CLI for product analytics and HogQL queries")


@app.command("health")
def health():
    """Assert posthog connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.events(limit=1)
        payload = {"ok": True, "tool": "posthog", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "posthog", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import PostHogClient

    return PostHogClient()


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def query(
    sql: str = typer.Argument(..., help="HogQL SQL query"),
    name: str = typer.Option(None, "--name", "-n", help="Query name for logging"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Execute a HogQL query."""
    client = get_client()

    try:
        result = client.query(sql, name=name)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    columns = result.get("columns", [])
    rows = result.get("results", [])

    if not rows:
        console.print("[yellow]No results[/]")
        return

    if markdown:
        print_markdown_table(columns, [[str(cell) for cell in row] for row in rows])
        return

    table = Table(title="Query Results")
    for col in columns:
        table.add_column(str(col), overflow="fold")

    for row in rows:
        table.add_row(*[str(cell) for cell in row])

    console.print(table)


@app.command()
def breakdown(
    property: str = typer.Argument("$browser", help="Property to breakdown by"),
    event: str = typer.Option(None, "--event", "-e", help="Filter by event name"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to look back"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get event breakdown by a property (e.g., $browser, $os, $pathname)."""
    client = get_client()

    try:
        result = client.breakdown(event=event, property=property, days=days, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    columns = result.get("columns", [])
    rows = result.get("results", [])

    if not rows:
        console.print("[yellow]No results[/]")
        return

    if markdown:
        print_markdown_table(columns, [[str(cell) for cell in row] for row in rows])
        return

    title = f"Breakdown by {property}"
    if event:
        title += f" (event: {event})"
    title += f" - Last {days} days"

    table = Table(title=title)
    table.add_column("Value", style="cyan")
    table.add_column("Count", style="yellow", justify="right")
    table.add_column("Percentage", style="green", justify="right")

    for row in rows:
        value, count, pct = row[0], row[1], row[2]
        table.add_row(str(value) if value else "(none)", str(count), f"{pct}%")

    console.print(table)


@app.command()
def pageviews(
    url: str = typer.Option(None, "--url", "-u", help="Filter URLs containing this pattern"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to look back"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get pageview analytics."""
    client = get_client()

    try:
        result = client.pageviews(url_pattern=url, days=days, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    rows = result.get("results", [])

    if not rows:
        console.print("[yellow]No pageviews found[/]")
        return

    if markdown:
        print_markdown_table(
            ["URL", "Views", "Unique Visitors"], [[str(cell) for cell in row] for row in rows]
        )
        return

    table = Table(title=f"Pageviews - Last {days} days")
    table.add_column("URL", style="cyan", max_width=60, overflow="fold")
    table.add_column("Views", style="yellow", justify="right")
    table.add_column("Unique Visitors", style="green", justify="right")

    for row in rows:
        table.add_row(str(row[0]), str(row[1]), str(row[2]))

    console.print(table)


@app.command("user-agents")
def user_agents(
    url: str = typer.Option(None, "--url", "-u", help="Filter URLs containing this pattern"),
    event: str = typer.Option("$pageview", "--event", "-e", help="Event type"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to look back"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get user-agent breakdown (browser + OS)."""
    client = get_client()

    try:
        result = client.user_agents(url_pattern=url, event=event, days=days, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    rows = result.get("results", [])

    if not rows:
        console.print("[yellow]No results[/]")
        return

    if markdown:
        print_markdown_table(
            ["Browser", "OS", "Count", "Percentage"],
            [[str(cell) for cell in row] for row in rows],
        )
        return

    title = f"User-Agent Breakdown - Last {days} days"
    if url:
        title += f" (URL: *{url}*)"

    table = Table(title=title)
    table.add_column("Browser", style="cyan")
    table.add_column("OS", style="blue")
    table.add_column("Count", style="yellow", justify="right")
    table.add_column("Percentage", style="green", justify="right")

    for row in rows:
        browser, os_name, count, pct = row
        table.add_row(
            str(browser) if browser else "(none)",
            str(os_name) if os_name else "(none)",
            str(count),
            f"{pct}%",
        )

    console.print(table)


@app.command()
def events(
    event: str = typer.Option(None, "--event", "-e", help="Filter by event name"),
    after: str = typer.Option(None, "--after", "-a", help="Events after (YYYY-MM-DD)"),
    before: str = typer.Option(None, "--before", "-b", help="Events before (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List recent events."""
    client = get_client()

    try:
        result = client.events(event=event, after=after, before=before, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    rows = result.get("results", [])

    if not rows:
        console.print("[yellow]No events found[/]")
        return

    table = Table(title="Events")
    table.add_column("Timestamp", style="dim")
    table.add_column("Event", style="cyan")
    table.add_column("Distinct ID", style="yellow", max_width=30, overflow="fold")

    for row in rows:
        ts, evt, distinct_id, _ = row
        table.add_row(str(ts)[:19], str(evt), str(distinct_id)[:30])

    console.print(table)


if __name__ == "__main__":
    app()
