"""CLI for the BrightData public web tool."""

from __future__ import annotations

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

from .client import BrightDataClient


app = typer.Typer(name="brightdata", help="BrightData public web search, discovery, and scraping")
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
    """Run a public web search via BrightData."""
    with _client() as c:
        _emit(c.search(query, engine=engine, cursor=cursor), as_json)


@app.command()
def discover(
    query: str = typer.Argument(..., help="Discovery query"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Discover URLs across the public web for a query."""
    with _client() as c:
        _emit(c.discover(query), as_json)


@app.command("scrape-markdown")
def scrape_markdown(
    url: str = typer.Argument(..., help="Public URL to scrape"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Fetch a public page and return Markdown."""
    with _client() as c:
        _emit(c.scrape_markdown(url), as_json)


@app.command("scrape-html")
def scrape_html(
    url: str = typer.Argument(..., help="Public URL to scrape"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Fetch a public page and return HTML."""
    with _client() as c:
        _emit(c.scrape_html(url), as_json)


@app.command("session-stats")
def session_stats(
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Return BrightData session usage statistics."""
    with _client() as c:
        _emit(c.session_stats(), as_json)


if __name__ == "__main__":
    app()
