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


# The prompt names the Slack destination for the turn under these keys, the same
# ones the `slack upload` example uses. A sandbox does NOT carry the thread in
# its environment: warm-pool pods are started before any thread claims them and
# pod env cannot be changed afterwards, so CENTAUR_THREAD_KEY is unset in
# practice. The caller passes the destination; env is only a fallback for a
# dedicated sandbox that happens to have it.
SESSION_CONTEXT_CHANNEL = "session_context.slack.channel_id"
SESSION_CONTEXT_THREAD = "session_context.slack.thread_ts"


def emit_or_reject(
    call,
    *,
    slack_card: bool = False,
    flat: bool = False,
    channel: str | None = None,
    thread: str | None = None,
) -> None:
    """Run a client call and print its result.

    The client pre-validates option contracts (enum choices, count ranges,
    strict/node_ids) and raises ValueError before any network call; the free
    MCP tier raises McpAuthRequired/McpRateLimited with actionable messages.
    Surface all of these as one-line CLI errors instead of tracebacks.

    When the result carries a card, the payload gains a `slack_card` entry: the
    post outcome with `slack_card=True`, otherwise a hint naming the command that
    renders it. The result is printed either way, because a failed post must not
    lose the data that was retrieved.
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
        note = _slack_card_note(payload, post=slack_card, flat=flat, channel=channel, thread=thread)
        if note:
            payload = {**payload, "slack_card": note}
    emit(payload)


def _slack_destination(channel: str | None, thread: str | None):
    """The channel/thread to post into: explicit values first, then the env key.

    Returns None when neither is available, which is the normal case in a
    warm-pool sandbox and is why the hint tells the caller to pass them.
    """
    from ._slack import ThreadTarget, thread_target

    if channel:
        return ThreadTarget(channel=channel, thread_ts=thread or "")
    target = thread_target()
    if target is None:
        return None
    return ThreadTarget(channel=target.channel, thread_ts=thread or target.thread_ts)


def _slack_card_note(
    payload: dict, *, post: bool, flat: bool, channel: str | None, thread: str | None
) -> dict | None:
    """Post the lead card, or hint at how to.

    Returns None when the result has no renderable card, so web-only results stay
    unchanged. It cannot tell which chat surface the turn belongs to (the sandbox
    is not told), so the hint is phrased conditionally rather than suppressed,
    which would mean never showing it at all.
    """
    from ._slack import (
        SlackPostError,
        card_message,
        cards_from_payload,
        post_message,
        pub_id_of,
    )

    cards = cards_from_payload(payload)
    if not cards:
        return None
    pub_id = pub_id_of(cards[0])
    target = _slack_destination(channel, thread)

    if not post:
        if target is None:
            hint = (
                "if this turn is in a Slack thread, show this chart with: "
                f"{TOOL_COMMAND} slack-card {pub_id} --post "
                f"--channel <{SESSION_CONTEXT_CHANNEL}> --thread <{SESSION_CONTEXT_THREAD}>"
                " (or re-run this command with --slack-card and the same two options)"
            )
        else:
            hint = (
                "to show this chart in the thread, re-run with --slack-card, or: "
                f"{TOOL_COMMAND} slack-card {pub_id} --post"
            )
        return {"posted": False, "hint": hint}

    if target is None:
        return {
            "posted": False,
            "error": (
                "no Slack destination: pass --channel "
                f"<{SESSION_CONTEXT_CHANNEL}> and --thread <{SESSION_CONTEXT_THREAD}> "
                "from the Slack Session Context in this turn's prompt"
            ),
        }

    body = card_message(
        cards[0],
        channel=target.channel,
        thread_ts=target.thread_ts or None,
        layout="flat" if flat else "container",
    )
    if body is None:  # pragma: no cover - cards_from_payload already validated
        return {"posted": False, "error": "the lead card cannot be rendered"}
    try:
        result = post_message(body)
    except SlackPostError as exc:
        # Non-fatal: the retrieved data is still worth printing.
        return {"posted": False, "error": str(exc)}
    return {
        "posted": True,
        "channel": result.get("channel") or target.channel,
        "ts": result.get("ts"),
        "layout": "flat" if flat else "container",
    }


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
        help="Also post the top card's chart into a Slack thread (needs --channel/--thread)",
    ),
    flat: bool = typer.Option(
        False, "--flat", help="With --slack-card: use flat blocks instead of a container"
    ),
    channel: str = typer.Option(
        None, help="Slack channel id for --slack-card (session_context.slack.channel_id)"
    ),
    thread: str = typer.Option(
        None, help="Slack thread ts for --slack-card (session_context.slack.thread_ts)"
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
        channel=channel,
        thread=thread,
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
        help="Also post the top card's chart into a Slack thread (needs --channel/--thread)",
    ),
    flat: bool = typer.Option(
        False, "--flat", help="With --slack-card: use flat blocks instead of a container"
    ),
    channel: str = typer.Option(
        None, help="Slack channel id for --slack-card (session_context.slack.channel_id)"
    ),
    thread: str = typer.Option(
        None, help="Slack thread ts for --slack-card (session_context.slack.thread_ts)"
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
        channel=channel,
        thread=thread,
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
    post: bool = typer.Option(
        False, "--post", help="Post into the current Slack thread instead of printing"
    ),
    flat: bool = typer.Option(
        False,
        "--flat",
        help="Use top-level image/context/actions blocks instead of a container",
    ),
    channel: str = typer.Option(None, help="Override the channel (defaults to this thread)"),
    thread: str = typer.Option(None, help="Override the thread timestamp"),
):
    """Render a Tako card as a Slack message.

    Presentation only: the chart image already exists on every card, so this
    makes no Tako API call. Slack image blocks scale to the message width and
    preserve aspect ratio, so the chart is never cropped, and the headline
    numbers and "Open in Tako" button sit outside the image where they cost no
    image height.

    Pipe a `search` or `answer` result in to keep the card's title, headline, and
    sources:

        datasearch search "nvidia revenue" | datasearch slack-card --post

    A bare pub id also works, but renders without that text. Prints the
    `chat.postMessage` payload unless `--post` is given, which sends into the
    Slack thread this turn belongs to and needs only `chat:write`
    (`chat:write.public` for public channels the bot has not joined; private
    channels need the bot invited). Use `--flat` if the container block renders
    badly on any client.
    """
    from ._slack import (
        SlackPostError,
        card_message,
        cards_from_payload,
        post_message,
        pub_id_of,
        thread_target,
    )

    source_card: dict | None = None
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

    body = card_message(
        source_card,
        channel=channel,
        thread_ts=thread,
        layout="flat" if flat else "container",
    )
    if body is None:  # pragma: no cover - the card id was already validated
        raise typer.BadParameter("card cannot be rendered")

    if not post:
        emit(body)
        return

    if not body.get("channel"):
        target = thread_target()
        if target is None:
            console.print(
                "[red]No Slack destination. Pass --channel "
                f"<{SESSION_CONTEXT_CHANNEL}> and --thread <{SESSION_CONTEXT_THREAD}> from the "
                "Slack Session Context in this turn's prompt, or drop --post to print the "
                "payload.[/]"
            )
            raise typer.Exit(1)
        body["channel"] = target.channel
        body.setdefault("thread_ts", target.thread_ts)

    try:
        result = post_message(body)
    except SlackPostError as exc:
        console.print(f"[red]{exc}[/]")
        raise typer.Exit(1) from exc
    emit(
        {
            "ok": True,
            "channel": result.get("channel") or body["channel"],
            "ts": result.get("ts"),
            "layout": "flat" if flat else "container",
        }
    )


if __name__ == "__main__":
    app()
