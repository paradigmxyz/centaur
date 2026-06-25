"""CLI for Google News RSS."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="googlenews", help="Google News CLI for news search and headlines")


@app.command("health")
def health():
    """Assert googlenews connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.headlines()
        payload = {"ok": True, "tool": "googlenews", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "googlenews", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import GoogleNewsClient

    return GoogleNewsClient()


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def truncate(text: str, length: int = 60) -> str:
    """Truncate text to specified length."""
    if len(text) <= length:
        return text
    return text[: length - 3] + "..."


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for news articles."""
    client = get_client()
    articles = client.search(query, limit=limit)

    if json_output:
        print(json.dumps(articles, indent=2))
        return

    if not articles:
        console.print(f"[yellow]No results for '{query}'[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for article in articles:
            rows.append(
                [
                    truncate(article["title"], 50),
                    article["source"] or "N/A",
                    article["published"][:16] if article["published"] else "N/A",
                ]
            )
        print_markdown_table(["Title", "Source", "Published"], rows)
        return

    table = Table(title=f"Search: '{query}'")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Source", style="yellow", max_width=20)
    table.add_column("Published", style="dim", max_width=20)

    for article in articles:
        table.add_row(
            truncate(article["title"], 50),
            article["source"] or "N/A",
            article["published"][:16] if article["published"] else "N/A",
        )

    console.print(table)


@app.command()
def headlines(
    country: str = typer.Option("US", "--country", "-c", help="Country code (e.g., US, GB, DE)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get top headlines."""
    client = get_client()
    articles = client.headlines(country=country, limit=limit)

    if json_output:
        print(json.dumps(articles, indent=2))
        return

    if not articles:
        console.print("[yellow]No headlines found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for article in articles:
            rows.append(
                [
                    truncate(article["title"], 50),
                    article["source"] or "N/A",
                    article["published"][:16] if article["published"] else "N/A",
                ]
            )
        print_markdown_table(["Title", "Source", "Published"], rows)
        return

    table = Table(title=f"Top Headlines ({country})")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Source", style="yellow", max_width=20)
    table.add_column("Published", style="dim", max_width=20)

    for article in articles:
        table.add_row(
            truncate(article["title"], 50),
            article["source"] or "N/A",
            article["published"][:16] if article["published"] else "N/A",
        )

    console.print(table)


@app.command()
def topic(
    topic_name: str = typer.Argument(
        ...,
        help="Topic: WORLD, NATION, BUSINESS, TECHNOLOGY, ENTERTAINMENT, SPORTS, SCIENCE, HEALTH",
    ),
    country: str = typer.Option("US", "--country", "-c", help="Country code"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get news by topic."""
    client = get_client()
    articles = client.topic(topic_name, country=country, limit=limit)

    if json_output:
        print(json.dumps(articles, indent=2))
        return

    if not articles:
        console.print(f"[yellow]No articles for topic '{topic_name}'[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for article in articles:
            rows.append(
                [
                    truncate(article["title"], 50),
                    article["source"] or "N/A",
                    article["published"][:16] if article["published"] else "N/A",
                ]
            )
        print_markdown_table(["Title", "Source", "Published"], rows)
        return

    table = Table(title=f"Topic: {topic_name.upper()}")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Source", style="yellow", max_width=20)
    table.add_column("Published", style="dim", max_width=20)

    for article in articles:
        table.add_row(
            truncate(article["title"], 50),
            article["source"] or "N/A",
            article["published"][:16] if article["published"] else "N/A",
        )

    console.print(table)


if __name__ == "__main__":
    app()
