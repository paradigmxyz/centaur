"""CLI for the BrightData public web tool."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import typer
from dotenv import load_dotenv
from rich.console import Console

_REPO_ROOT = Path(__file__).resolve().parents[3]
if (_REPO_ROOT / "centaur_sdk").exists():
    sys.path.insert(0, str(_REPO_ROOT))

try:
    from .client import BrightDataClient
except ImportError:  # pragma: no cover - used by installed console script layout
    from client import BrightDataClient

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


@app.command()
def discover(
    query: str = typer.Argument(..., help="Discovery query"),
    intent: str | None = typer.Option(None, "--intent", help="Ranking intent"),
    filter_keyword: list[str] | None = typer.Option(  # noqa: B008
        None, "--filter-keyword", help="Keyword to prioritize; repeatable"
    ),
    num_results: int = typer.Option(10, "--num-results", "-n", help="Number of results"),
    include_content: bool = typer.Option(False, "--include-content", help="Include page content"),
    include_images: bool = typer.Option(False, "--include-images", help="Include images"),
    country: str = typer.Option("US", "--country", help="ISO country code"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Run a BrightData Discover search and wait for ranked results."""
    with _client() as c:
        _emit(
            c.discover(
                query,
                intent=intent,
                filter_keywords=filter_keyword,
                num_results=num_results,
                include_content=include_content,
                include_images=include_images,
                country=country,
            ),
            as_json,
        )


@app.command("discover-result")
def discover_result(
    task_id: str = typer.Argument(..., help="BrightData Discover task id"),
    as_json: bool = typer.Option(False, "--json", help="Emit raw JSON"),
) -> None:
    """Fetch results for a BrightData Discover task."""
    with _client() as c:
        _emit(c.discover_result(task_id), as_json)


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
    """Return per-zone bandwidth stats for the SERP and Unlocker zones."""
    with _client() as c:
        _emit(c.session_stats(), as_json)


if __name__ == "__main__":
    app()
