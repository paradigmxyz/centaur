"""CLI for The Block RSS."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(name="theblock", help="The Block CLI for crypto news")


@app.command("health")
def health():
    """Assert theblock connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.news(limit=1)
        payload = {"ok": True, "tool": "theblock", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "theblock", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import TheBlockClient

    return TheBlockClient()


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
    """Get latest crypto news from The Block."""
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
            author = article["author"] or "N/A"
            rows.append([truncate(article["title"], 50), author, published])
        print_markdown_table(["Title", "Author", "Published"], rows)
        return

    table = Table(title="The Block Latest News")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Author", style="yellow", max_width=15)
    table.add_column("Published", style="dim", max_width=18)

    for article in articles:
        published = article["published"][:16] if article["published"] else "N/A"
        author = article["author"] or "N/A"
        table.add_row(truncate(article["title"], 50), author, published)

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
            author = article["author"] or "N/A"
            rows.append([truncate(article["title"], 50), author, published])
        print_markdown_table(["Title", "Author", "Published"], rows)
        return

    table = Table(title=f"The Block Search: '{query}'")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Author", style="yellow", max_width=15)
    table.add_column("Published", style="dim", max_width=18)

    for article in articles:
        published = article["published"][:16] if article["published"] else "N/A"
        author = article["author"] or "N/A"
        table.add_row(truncate(article["title"], 50), author, published)

    console.print(table)


if __name__ == "__main__":
    app()
