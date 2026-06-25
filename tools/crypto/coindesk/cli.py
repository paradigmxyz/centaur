"""CLI for CoinDesk RSS."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

from centaur_sdk import Table

from .client import CoinDeskClient

app = typer.Typer(name="coindesk", help="CoinDesk CLI for crypto news")


@app.command("health")
def health():
    """Assert coindesk connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.news(limit=1)
        payload = {"ok": True, "tool": "coindesk", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "coindesk", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    return CoinDeskClient()


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def truncate(text: str, length: int = 60) -> str:
    """Truncate text to specified length."""
    if not text:
        return ""
    if len(text) <= length:
        return text
    return text[: length - 3] + "..."


@app.command()
def news(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get latest crypto news."""
    client = get_client()
    articles = client.news(limit=limit)

    if json_output:
        print(json.dumps(articles, indent=2))
        return

    if not articles:
        console.print("[yellow]No articles found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for article in articles:
            published = article["published"][:16] if article["published"] else "N/A"
            tags = ", ".join(article["tags"][:3]) if article["tags"] else ""
            rows.append([truncate(article["title"], 50), published, tags])
        print_markdown_table(["Title", "Published", "Tags"], rows)
        return

    table = Table(title="CoinDesk Latest News")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Published", style="dim", max_width=18)
    table.add_column("Tags", style="yellow", max_width=20)

    for article in articles:
        published = article["published"][:16] if article["published"] else "N/A"
        tags = ", ".join(article["tags"][:3]) if article["tags"] else ""
        table.add_row(truncate(article["title"], 50), published, tags)

    console.print(table)


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search crypto news by keyword."""
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
            published = article["published"][:16] if article["published"] else "N/A"
            tags = ", ".join(article["tags"][:3]) if article["tags"] else ""
            rows.append([truncate(article["title"], 50), published, tags])
        print_markdown_table(["Title", "Published", "Tags"], rows)
        return

    table = Table(title=f"CoinDesk Search: '{query}'")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Published", style="dim", max_width=18)
    table.add_column("Tags", style="yellow", max_width=20)

    for article in articles:
        published = article["published"][:16] if article["published"] else "N/A"
        tags = ", ".join(article["tags"][:3]) if article["tags"] else ""
        table.add_row(truncate(article["title"], 50), published, tags)

    console.print(table)


if __name__ == "__main__":
    app()
