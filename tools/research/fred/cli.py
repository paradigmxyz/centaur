"""Command-line interface for the FRED API."""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from typing import Any

import httpx
import typer

from .client import FredClient, _client

app = typer.Typer(
    name="fred",
    help="FRED (Federal Reserve Economic Data): search series, fetch observations, "
    "and browse categories, releases, sources, and tags.",
    no_args_is_help=True,
)

# FRED API keys are 32-character lowercase alphanumeric strings.
_KEY_PATTERN = re.compile(r"\b[a-z0-9]{32}\b")

Limit = typer.Option(20, "--limit", "-n", help="Maximum results.")
Offset = typer.Option(0, "--offset", help="Result offset for pagination.")


def _echo(payload: dict[str, Any]) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))


def _error_message(exc: Exception) -> str:
    if isinstance(exc, httpx.HTTPStatusError):
        message = f"FRED API error: HTTP {exc.response.status_code}"
        # FRED reports bad parameters (e.g. unknown series) as a JSON error_message.
        # Surface only that field, with anything shaped like an API key redacted.
        try:
            detail = exc.response.json().get("error_message")
        except (ValueError, AttributeError):
            detail = None
        if isinstance(detail, str) and detail:
            message += f": {_KEY_PATTERN.sub('[redacted]', detail)[:300]}"
        return message
    if isinstance(exc, httpx.HTTPError):
        return f"FRED request failed ({type(exc).__name__})."
    return str(exc)


def _run(call: Callable[[FredClient], dict[str, Any]]) -> None:
    try:
        with _client() as client:
            _echo(call(client))
    except (httpx.HTTPError, RuntimeError, ValueError) as exc:
        typer.echo(_error_message(exc), err=True)
        raise typer.Exit(1) from None


@app.command()
def health() -> None:
    """Check authentication and connectivity."""
    try:
        with _client() as client:
            series = client.get_series("GDP").get("seriess") or [{}]
    except (httpx.HTTPError, RuntimeError, ValueError) as exc:
        _echo({"ok": False, "tool": "fred", "error": _error_message(exc), "details": {}})
        raise typer.Exit(1) from None
    details = {"series_id": series[0].get("id"), "last_updated": series[0].get("last_updated")}
    _echo({"ok": True, "tool": "fred", "error": None, "details": details})


@app.command()
def search(
    text: str = typer.Argument(..., help="Search keywords, e.g. 'unemployment rate'."),
    search_type: str = typer.Option(
        "full_text", "--type", help="full_text or series_id (supports * wildcards)."
    ),
    order_by: str | None = typer.Option(
        None,
        "--order-by",
        help="search_rank, series_id, title, units, frequency, popularity, last_updated, ...",
    ),
    sort_order: str | None = typer.Option(None, "--sort", help="asc or desc."),
    tags: str | None = typer.Option(None, "--tags", help="Semicolon-separated tag filter."),
    limit: int = Limit,
    offset: int = Offset,
) -> None:
    """Search series by keywords."""
    _run(
        lambda c: c.search_series(
            text,
            search_type=search_type,
            limit=limit,
            offset=offset,
            order_by=order_by,
            sort_order=sort_order,
            tag_names=tags,
        )
    )


@app.command()
def series(
    series_id: str = typer.Argument(..., help="Series ID, e.g. GDP, UNRATE, CPIAUCSL."),
) -> None:
    """Get series metadata."""
    _run(lambda c: c.get_series(series_id))


@app.command()
def observations(
    series_id: str = typer.Argument(..., help="Series ID, e.g. UNRATE."),
    start: str | None = typer.Option(None, "--start", help="Start date YYYY-MM-DD."),
    end: str | None = typer.Option(None, "--end", help="End date YYYY-MM-DD."),
    units: str | None = typer.Option(
        None, "--units", help="lin, chg, ch1, pch, pc1, pca, cch, cca, or log."
    ),
    frequency: str | None = typer.Option(
        None, "--frequency", help="Aggregate to d, w, bw, m, q, sa, or a."
    ),
    aggregation: str | None = typer.Option(
        None, "--aggregation", help="avg, sum, or eop (with --frequency)."
    ),
    limit: int | None = typer.Option(None, "--limit", "-n", help="Maximum observations."),
    latest: bool = typer.Option(False, "--latest", help="Return newest observations first."),
) -> None:
    """Get data values for a series."""
    _run(
        lambda c: c.get_observations(
            series_id,
            start=start,
            end=end,
            units=units,
            frequency=frequency,
            aggregation_method=aggregation,
            limit=limit,
            sort_order="desc" if latest else "asc",
        )
    )


@app.command("series-categories")
def series_categories(series_id: str = typer.Argument(..., help="Series ID.")) -> None:
    """Get the categories for a series."""
    _run(lambda c: c.get_series_categories(series_id))


@app.command("series-release")
def series_release(series_id: str = typer.Argument(..., help="Series ID.")) -> None:
    """Get the release for a series."""
    _run(lambda c: c.get_series_release(series_id))


@app.command("series-tags")
def series_tags(series_id: str = typer.Argument(..., help="Series ID.")) -> None:
    """Get the tags for a series."""
    _run(lambda c: c.get_series_tags(series_id))


@app.command()
def updates(limit: int = Limit, offset: int = Offset) -> None:
    """List recently updated series."""
    _run(lambda c: c.get_series_updates(limit=limit, offset=offset))


@app.command()
def category(category_id: int = typer.Argument(0, help="Category ID (0 is the root).")) -> None:
    """Get a category."""
    _run(lambda c: c.get_category(category_id))


@app.command("category-children")
def category_children(
    category_id: int = typer.Argument(0, help="Category ID (0 is the root)."),
) -> None:
    """List child categories."""
    _run(lambda c: c.get_category_children(category_id))


@app.command("category-series")
def category_series(
    category_id: int = typer.Argument(..., help="Category ID."),
    limit: int = Limit,
    offset: int = Offset,
) -> None:
    """List series in a category."""
    _run(lambda c: c.get_category_series(category_id, limit=limit, offset=offset))


@app.command()
def releases(limit: int = Limit, offset: int = Offset) -> None:
    """List releases of economic data."""
    _run(lambda c: c.list_releases(limit=limit, offset=offset))


@app.command()
def release(release_id: int = typer.Argument(..., help="Release ID.")) -> None:
    """Get a release."""
    _run(lambda c: c.get_release(release_id))


@app.command("release-series")
def release_series(
    release_id: int = typer.Argument(..., help="Release ID."),
    limit: int = Limit,
    offset: int = Offset,
) -> None:
    """List series in a release."""
    _run(lambda c: c.get_release_series(release_id, limit=limit, offset=offset))


@app.command("release-dates")
def release_dates(
    release_id: int | None = typer.Argument(None, help="Release ID; omit for all releases."),
    start: str | None = typer.Option(None, "--start", help="Start date YYYY-MM-DD."),
    end: str | None = typer.Option(None, "--end", help="End date YYYY-MM-DD."),
    include_future: bool = typer.Option(
        False, "--include-future", help="Include scheduled dates that have no data yet."
    ),
    sort_order: str | None = typer.Option(None, "--sort", help="asc or desc."),
    limit: int = Limit,
) -> None:
    """Get release dates (the economic data calendar)."""
    _run(
        lambda c: c.get_release_dates(
            release_id,
            start=start,
            end=end,
            limit=limit,
            include_future=include_future,
            sort_order=sort_order,
        )
    )


@app.command()
def sources(limit: int = Limit, offset: int = Offset) -> None:
    """List data sources."""
    _run(lambda c: c.list_sources(limit=limit, offset=offset))


@app.command()
def tags(
    search_text: str | None = typer.Argument(None, help="Optional tag search text."),
    limit: int = Limit,
    offset: int = Offset,
) -> None:
    """List or search tags."""
    _run(lambda c: c.list_tags(search_text, limit=limit, offset=offset))


@app.command("tag-series")
def tag_series(
    tag_names: str = typer.Argument(..., help="Semicolon-separated tags, e.g. 'usa;inflation'."),
    limit: int = Limit,
    offset: int = Offset,
) -> None:
    """List series matching all given tags."""
    _run(lambda c: c.get_tag_series(tag_names, limit=limit, offset=offset))


if __name__ == "__main__":
    app()
