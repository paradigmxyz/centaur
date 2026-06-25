"""CLI for NewsAPI.org."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="newsapi", help="NewsAPI CLI for news search and headlines")


@app.command("health")
def health():
    """Assert newsapi connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.sources()
        payload = {"ok": True, "tool": "newsapi", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "newsapi", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import NewsAPIClient

    return NewsAPIClient()


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
def headlines(
    country: str = typer.Option("us", "--country", "-c", help="Country code (e.g., us, gb, de)"),
    category: str = typer.Option(
        None,
        "--category",
        "-cat",
        help="Category: business, entertainment, general, health, science, sports, technology",
    ),
    sources: str = typer.Option(None, "--sources", "-s", help="Comma-separated source IDs"),
    query: str = typer.Option(None, "--query", "-q", help="Keywords to search"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get top headlines."""
    client = get_client()
    # Can't mix sources with country/category
    if sources:
        data = client.headlines(sources=sources, q=query, page_size=limit)
    else:
        data = client.headlines(country=country, category=category, q=query, page_size=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    articles = data.get("articles", [])
    if not articles:
        console.print("[yellow]No headlines found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for article in articles:
            source = article.get("source", {}).get("name", "N/A")
            published = article.get("publishedAt", "")[:10]
            rows.append([truncate(article.get("title", ""), 50), source, published])
        print_markdown_table(["Title", "Source", "Published"], rows)
        return

    table = Table(title="Top Headlines")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Source", style="yellow", max_width=20)
    table.add_column("Published", style="dim", max_width=12)

    for article in articles:
        source = article.get("source", {}).get("name", "N/A")
        published = article.get("publishedAt", "")[:10]
        table.add_row(truncate(article.get("title", ""), 50), source, published)

    console.print(table)


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    sources: str = typer.Option(None, "--sources", "-s", help="Comma-separated source IDs"),
    domains: str = typer.Option(None, "--domains", "-d", help="Comma-separated domains"),
    from_date: str = typer.Option(None, "--from", help="From date (YYYY-MM-DD)"),
    to_date: str = typer.Option(None, "--to", help="To date (YYYY-MM-DD)"),
    language: str = typer.Option(None, "--lang", "-l", help="Language code (e.g., en, de, fr)"),
    sort_by: str = typer.Option(
        "publishedAt", "--sort", help="Sort by: relevancy, popularity, publishedAt"
    ),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search all articles."""
    client = get_client()
    data = client.search(
        q=query,
        sources=sources,
        domains=domains,
        from_date=from_date,
        to_date=to_date,
        language=language,
        sort_by=sort_by,
        page_size=limit,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    articles = data.get("articles", [])
    if not articles:
        console.print(f"[yellow]No results for '{query}'[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for article in articles:
            source = article.get("source", {}).get("name", "N/A")
            published = article.get("publishedAt", "")[:10]
            rows.append([truncate(article.get("title", ""), 50), source, published])
        print_markdown_table(["Title", "Source", "Published"], rows)
        return

    table = Table(title=f"Search: '{query}'")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Source", style="yellow", max_width=20)
    table.add_column("Published", style="dim", max_width=12)

    for article in articles:
        source = article.get("source", {}).get("name", "N/A")
        published = article.get("publishedAt", "")[:10]
        table.add_row(truncate(article.get("title", ""), 50), source, published)

    console.print(table)
    console.print(f"\n[dim]Total results: {data.get('totalResults', 0)}[/]")


@app.command()
def sources(
    category: str = typer.Option(
        None,
        "--category",
        "-cat",
        help="Category: business, entertainment, general, health, science, sports, technology",
    ),
    language: str = typer.Option(None, "--lang", "-l", help="Language code (e.g., en, de)"),
    country: str = typer.Option(None, "--country", "-c", help="Country code (e.g., us, gb)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List available news sources."""
    client = get_client()
    data = client.sources(category=category, language=language, country=country)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    sources_list = data.get("sources", [])
    if not sources_list:
        console.print("[yellow]No sources found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for src in sources_list:
            rows.append(
                [
                    src.get("id", ""),
                    src.get("name", ""),
                    src.get("category", ""),
                    src.get("country", "").upper(),
                ]
            )
        print_markdown_table(["ID", "Name", "Category", "Country"], rows)
        return

    table = Table(title="News Sources")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Category", style="yellow")
    table.add_column("Country", style="green")

    for src in sources_list:
        table.add_row(
            src.get("id", ""),
            src.get("name", ""),
            src.get("category", ""),
            src.get("country", "").upper(),
        )

    console.print(table)


if __name__ == "__main__":
    app()
