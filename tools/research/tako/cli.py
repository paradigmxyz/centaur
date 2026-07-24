"""CLI for the Tako API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

app = typer.Typer(
    name="tako",
    help=(
        "Search Tako's proprietary licensed-source data and the web in one "
        "call; returns structured data cards, cited answers, and exportable "
        "datasets. The free `available-data` can confirm what data exists "
        "before a priced `search`/`answer`."
    ),
)


@app.command("health")
def health():
    """Assert tako connectivity and auth with a safe read-only check."""
    from .client import _client, _dump

    client = _client()
    try:
        # graph search is the cheapest authenticated read; it exercises the
        # credential path without spending a priced search or answer call.
        details = _dump(client._graph_search("nvidia", limit=1))
        payload = {"ok": True, "tool": "tako", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "tako", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import TakoClient

    return TakoClient()


def emit(data) -> None:
    """Print a result as indented JSON."""
    print(json.dumps(data, indent=2, ensure_ascii=False, default=str))


@app.command()
def search(
    query: str = typer.Argument(..., help="Natural language query"),
    effort: str = typer.Option(None, help="instant, fast (default), or deep"),
    data_count: int = typer.Option(None, help="Max data cards, 1-20. 0 skips the data index"),
    web_count: int = typer.Option(None, help="Max web results, 1-20. 0 skips the web index"),
    node_id: list[str] = typer.Option(  # noqa: B008
        None,
        "--node-id",
        help="Pin a graph node id from `available-data` as a retrieval candidate (repeatable, max 20)",
    ),
    strict: bool = typer.Option(False, help="Return only cards matching --node-id"),
    country_code: str = typer.Option(None, help="ISO 3166-1 alpha-2 code"),
    locale: str = typer.Option(None, help="BCP-47 locale tag"),
):
    """Search Tako for structured data cards and web results."""
    emit(
        get_client().search(
            query,
            effort=effort,
            data_count=data_count,
            web_count=web_count,
            node_ids=list(node_id) if node_id else None,
            strict=strict,
            country_code=country_code,
            locale=locale,
        )
    )


@app.command()
def answer(
    query: str = typer.Argument(..., help="Natural language question"),
    effort: str = typer.Option(None, help="instant, fast (default), or deep"),
    data_count: int = typer.Option(None, help="Max data cards, 1-20. 0 skips the data index"),
    web_count: int = typer.Option(None, help="Max web results, 1-20. 0 skips the web index"),
    node_id: list[str] = typer.Option(  # noqa: B008
        None,
        "--node-id",
        help="Pin a graph node id from `available-data` as a retrieval candidate (repeatable, max 20)",
    ),
    strict: bool = typer.Option(False, help="Return only cards matching --node-id"),
    country_code: str = typer.Option(None, help="ISO 3166-1 alpha-2 code"),
    locale: str = typer.Option(None, help="BCP-47 locale tag"),
):
    """Get a synthesized answer with the cards that support it."""
    emit(
        get_client().answer(
            query,
            effort=effort,
            data_count=data_count,
            web_count=web_count,
            node_ids=list(node_id) if node_id else None,
            strict=strict,
            country_code=country_code,
            locale=locale,
        )
    )


@app.command()
def contents(
    url: str = typer.Argument(..., help="A card webpage_url or a web result url"),
    mode: str = typer.Option(None, help="url (default, presigned link) or inline"),
    content_format: str = typer.Option(None, help="csv (default), json_records, or json_compact"),
    max_rows: int = typer.Option(None, help="Row cap for card exports. Free allowance is 20"),
    max_chars: int = typer.Option(None, help="Character cap on extracted web text"),
    quote_only: bool = typer.Option(False, help="Price the export without fetching or charging"),
):
    """Fetch the underlying data behind a result URL."""
    emit(
        get_client().contents(
            url,
            mode=mode,
            content_format=content_format,
            max_rows=max_rows,
            max_chars=max_chars,
            quote_only=quote_only,
        )
    )


NER_LABELS = (
    "PERSON", "ORG", "GPE", "LOC", "PRODUCT", "EVENT",
    "LANGUAGE", "MONEY", "METRIC", "STOCK_TICKER", "WEBSITE",
)


@app.command("available-data")
def available_data(
    q: str = typer.Argument(..., help="Entity or metric name to look up (min 2 chars)"),
    types: str = typer.Option(
        None, help="Narrow to 'entity' (a thing) or 'metric' (a measure). Omit to search both"
    ),
    label: str = typer.Option(
        None, help=f"NER label to prefer (boost, not filter): {', '.join(NER_LABELS)}"
    ),
):
    """Find what proprietary data Tako has on something. Free and fast.

    A good first step for a data lookup: run it before `search` or `answer`
    whenever you're unsure the data exists or what it's called. It is free,
    it confirms coverage, and the exact names it returns make the priced
    follow-up land precisely instead of guessing. Skip it when you already
    know the data exists or the query leans on web results.

    Works on an entity or a metric. An entity (a company, person, or place,
    e.g. Tesla) reports the metrics tracked on it; a metric (e.g. Inflation
    Rate) reports the entities it is tracked across. One metric across many
    entities is one metric-first call, and one entity across many metrics is
    one entity-first call. Never loop one call per name.

    Reuse each match's coverage.names verbatim in a follow-up `search` (e.g.
    "Tesla, Inc. Revenue"), optionally pinning its node_id with --node-id.
    """
    if len(q.strip()) < 2:
        raise typer.BadParameter("q must be at least 2 characters")
    if types is not None and types not in ("entity", "metric"):
        raise typer.BadParameter("--types must be 'entity' or 'metric'")
    if label is not None and label not in NER_LABELS:
        raise typer.BadParameter(f"--label must be one of: {', '.join(NER_LABELS)}")
    emit(get_client().available_data(q, types=types, label=label))


if __name__ == "__main__":
    app()
