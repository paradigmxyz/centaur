"""CLI for the BrightData public web tool."""

from __future__ import annotations

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from .client import BrightDataClient

load_dotenv()

app = typer.Typer(name="brightdata", help="BrightData public web search and scraping")
console = Console()


def _emit(payload: object, as_json: bool) -> None:
    if as_json:
        console.print_json(json.dumps(payload, default=str))
    else:
        console.print(payload)


def _client() -> BrightDataClient:
    return BrightDataClient()


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    engine: str = typer.Option("google", "--engine", "-e", help="google | bing | yandex"),
    cursor: str | None = typer.Option(None, "--cursor", help="Pagination cursor"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Run a public web search via BrightData's SERP zone."""
    with _client() as c:
        _emit(c.search(query, engine=engine, cursor=cursor), as_json)


@app.command("scrape-markdown")
def scrape_markdown(
    url: str = typer.Argument(..., help="Public URL to scrape"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Fetch a public page via Web Unlocker and return Markdown."""
    with _client() as c:
        _emit(c.scrape_markdown(url), as_json)


@app.command("scrape-html")
def scrape_html(
    url: str = typer.Argument(..., help="Public URL to scrape"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Fetch a public page via Web Unlocker and return HTML."""
    with _client() as c:
        _emit(c.scrape_html(url), as_json)


@app.command("session-stats")
def session_stats(
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Return per-zone usage statistics for the SERP and Unlocker zones."""
    with _client() as c:
        _emit(c.session_stats(), as_json)


if __name__ == "__main__":
    app()
