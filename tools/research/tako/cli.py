"""CLI for the Tako API."""

import json
import sys

import typer
from dotenv import load_dotenv
from rich.console import Console

from ._coverage import DOMAINS, NER_LABELS, NODE_TYPES, TOOL_COMMAND

load_dotenv()

app = typer.Typer(
    name=TOOL_COMMAND,
    help=(
        "Web search along with structured, cited, chart-backed data across "
        f"{'; '.join(DOMAINS)}. Instant answers via Tako. Run "
        f"`{TOOL_COMMAND} available-data <name>` to check coverage of a specific "
        "entity or metric; it is free."
    ),
)


@app.command("health")
def health():
    """Assert tako connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        # The cheapest read for the active backend: a one-node graph search
        # with a key, a free MCP available-data call without one. Neither
        # spends a priced search or answer call.
        details = client.probe()
        payload = {"ok": True, "tool": TOOL_COMMAND, "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": TOOL_COMMAND, "error": str(exc), "details": {}}
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


def emit_or_reject(call, *, slack_card: bool = False, flat: bool = False) -> None:
    """Run a client call and print its result.

    The client pre-validates option contracts (enum choices, count ranges,
    strict/node_ids) and raises ValueError before any network call; the free
    MCP tier raises McpAuthRequired/McpRateLimited with actionable messages.
    Surface all of these as one-line CLI errors instead of tracebacks.

    When the result carries a card, the payload gains a `slack_card` entry: the
    ready-to-post message with `slack_card=True`, otherwise a hint naming the
    pipeline that posts it.
    """
    from ._mcp import McpAuthRequired, McpRateLimited

    try:
        payload = call()
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc
    except (McpAuthRequired, McpRateLimited) as exc:
        console.print(f"[red]{exc}[/]")
        raise typer.Exit(1) from exc

    if isinstance(payload, dict):
        note = _slack_card_note(payload, include_message=slack_card, flat=flat)
        if note:
            # First key, not last. A search result runs well past a hundred lines
            # of JSON, and callers routinely pipe this through `head -N`, so
            # anything appended to the end is truncated away before it is read.
            payload = {"slack_card": note, **payload}
    emit(payload)


def _slack_card_note(payload: dict, *, include_message: bool, flat: bool) -> dict | None:
    """The lead card's Slack message, or a hint naming how to post it.

    Returns None when the result has no renderable card, so web-only results stay
    unchanged. This tool renders but never posts, so `include_message` only
    decides whether the payload rides along; either way the hint names the
    pipeline. It cannot tell which chat surface the turn belongs to (the sandbox
    is not told), so the hint is phrased conditionally.
    """
    from ._slack import card_message, cards_from_payload, pub_id_of

    cards = cards_from_payload(payload)
    if not cards:
        return None
    pub_id = pub_id_of(cards[0])
    note: dict = {
        "hint": (
            "if this turn is in a Slack thread, post this chart with: "
            f"{TOOL_COMMAND} slack-card {pub_id}"
            ' | slack send <session_context.slack.channel_id> "<fallback text>"'
            " --thread <session_context.slack.thread_ts> --blocks-json -"
        )
    }
    if include_message:
        note["message"] = card_message(cards[0], layout="flat" if flat else "container")
    return note


@app.command()
def search(
    query: str = typer.Argument(..., help="Natural language query"),
    effort: str = typer.Option(
        None, help="instant, fast (default), or deep (deep requires TAKO_API_KEY)"
    ),
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
    slack_card: bool = typer.Option(
        False,
        "--slack-card",
        help="Include the top card rendered as a Slack message, ready to pipe to `slack send`",
    ),
    flat: bool = typer.Option(
        False, "--flat", help="With --slack-card: use flat blocks instead of a container"
    ),
):
    """Search Tako for structured data cards and web results."""
    emit_or_reject(
        lambda: get_client().search(
            query,
            effort=effort,
            data_count=data_count,
            web_count=web_count,
            node_ids=list(node_id) if node_id else None,
            strict=strict,
            country_code=country_code,
            locale=locale,
        ),
        slack_card=slack_card,
        flat=flat,
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
    slack_card: bool = typer.Option(
        False,
        "--slack-card",
        help="Include the top card rendered as a Slack message, ready to pipe to `slack send`",
    ),
    flat: bool = typer.Option(
        False, "--flat", help="With --slack-card: use flat blocks instead of a container"
    ),
):
    """Get a synthesized answer with the cards that support it."""
    emit_or_reject(
        lambda: get_client().answer(
            query,
            effort=effort,
            data_count=data_count,
            web_count=web_count,
            node_ids=list(node_id) if node_id else None,
            strict=strict,
            country_code=country_code,
            locale=locale,
        ),
        slack_card=slack_card,
        flat=flat,
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
    """Fetch the underlying data behind a result URL. Requires TAKO_API_KEY."""
    emit_or_reject(
        lambda: get_client().contents(
            url,
            mode=mode,
            content_format=content_format,
            max_rows=max_rows,
            max_chars=max_chars,
            quote_only=quote_only,
        )
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
    if types is not None and types not in NODE_TYPES:
        raise typer.BadParameter(f"--types must be one of: {', '.join(NODE_TYPES)}")
    if label is not None and label not in NER_LABELS:
        raise typer.BadParameter(f"--label must be one of: {', '.join(NER_LABELS)}")
    emit_or_reject(lambda: get_client().available_data(q, types=types, label=label))


@app.command("slack-card")
def slack_card(
    card: str = typer.Argument(
        None, help="A card's pub id or URL. Omit to read a search result from stdin"
    ),
    index: int = typer.Option(0, help="Which card to render when several were piped in"),
    flat: bool = typer.Option(
        False,
        "--flat",
        help="Use top-level image/context/actions blocks instead of a container",
    ),
):
    """Render a Tako card as a Slack message and print it.

    Rendering only: the chart image already exists on every card, so this makes
    no Tako API call, and it never posts. Pipe the payload to the `slack` tool,
    which owns the Slack credential:

        datasearch slack-card <card> | slack send <channel> "<fallback>" --blocks-json -

    Pipe a `search` or `answer` result in to keep the card's title, headline, and
    sources; a bare pub id also works but renders without that text:

        datasearch search "nvidia revenue" | datasearch slack-card

    Slack image blocks scale to the message width and preserve aspect ratio, so
    the chart is never cropped, and the headline and "Open in Tako" button sit
    outside the image where they cost no image height. Use `--flat` if the
    container block renders badly on any client.
    """
    from ._slack import card_message, cards_from_payload, pub_id_of

    if card:
        pub_id = pub_id_of({"card_id": card, "webpage_url": card})
        if not pub_id:
            raise typer.BadParameter(f"not a Tako card id or URL: {card}")
        source_card = {"card_id": pub_id}
    else:
        if sys.stdin.isatty():
            raise typer.BadParameter(
                "pass a card id or URL, or pipe a `search`/`answer` result on stdin"
            )
        raw = sys.stdin.read().strip()
        if not raw:
            raise typer.BadParameter("no card given and stdin was empty")
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise typer.BadParameter(f"stdin is not valid JSON: {exc}") from exc
        cards = cards_from_payload(payload)
        if not cards:
            raise typer.BadParameter("no renderable Tako cards in the piped result")
        if not 0 <= index < len(cards):
            raise typer.BadParameter(f"--index {index} out of range: {len(cards)} card(s) piped in")
        source_card = cards[index]

    body = card_message(source_card, layout="flat" if flat else "container")
    if body is None:  # pragma: no cover - the card id was already validated
        raise typer.BadParameter("card cannot be rendered")
    emit(body)


if __name__ == "__main__":
    app()
