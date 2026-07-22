"""CLI for Slack search and analysis."""

import json
import re

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(name="slack", help="Slack CLI for AI agents")


@app.command("health")
def health():
    """Assert slack connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_bot_channels(limit=1)
        payload = {"ok": True, "tool": "slack", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "slack", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()
stderr_console = Console(stderr=True)

_SLACK_CHANNEL_ID_RE = re.compile(r"^[CGD][A-Z0-9]{8,}$")


def _channel_arg_is_id(channel: str) -> bool:
    value = channel.strip()
    if value.startswith("<#") and value.endswith(">"):
        value = value[2:-1].split("|", 1)[0]
    elif value.startswith("#"):
        value = value[1:]
    return bool(_SLACK_CHANNEL_ID_RE.fullmatch(value.upper()))


@app.command()
def send(
    channel: str = typer.Argument(..., help="Channel name, channel ID, or Slack user ID"),
    message: str = typer.Argument(..., help="Message text to send"),
    thread: str = typer.Option(None, "--thread", "-t", help="Thread timestamp to reply to"),
    no_attribution: bool = typer.Option(
        False,
        "--no-attribution",
        help="Skip auto-adding requester attribution (from SLACK_REQUESTER_ID)",
    ),
):
    """Send a message to a channel or Slack user DM.

    Examples:
        slack send "#eng-ai" "Hello from the CLI!"
        slack send eng-ai "Reply in thread" --thread 1234567890.123456
        slack send U12345678 "Direct follow-up"
    """
    from .client import send_message

    try:
        result = send_message(channel, message, thread_ts=thread, no_attribution=no_attribution)
        console.print("[green]✓ Message sent[/]")
        console.print(f"[dim]{result['permalink']}[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def dm(
    user_id: str = typer.Argument(..., help="Slack user ID, e.g. U12345678"),
    message: str = typer.Argument(..., help="Message text to send"),
    no_attribution: bool = typer.Option(
        False,
        "--no-attribution",
        help="Skip auto-adding requester attribution (from SLACK_REQUESTER_ID)",
    ),
):
    """Send a direct message to a Slack user.

    Examples:
        slack dm U12345678 "Remember to update the CRM"
    """
    from .client import send_dm

    try:
        result = send_dm(user_id, message, no_attribution=no_attribution)
        console.print("[green]✓ DM sent[/]")
        console.print(f"[dim]{result['permalink']}[/]")
    except (RuntimeError, ValueError) as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def search(
    query: str = typer.Argument(..., help="Text to search for (supports multiple terms)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full message text"),
    channels: str = typer.Option(
        None, "--channels", "-c", help="Comma-separated channel names to search"
    ),
    from_user: str = typer.Option(None, "--from", help="Filter by username"),
    depth: int = typer.Option(200, "--depth", "-d", help="Messages per channel to scan"),
):
    """Search messages in bot-accessible channels.

    Searches across Slack native search first, then scans channel history through
    the Centaur API server proxy. Results are ranked by relevance (exact phrase
    matches score higher). Use --channels to limit scope.

    Note: Only searches channels where the bot is a member. To search more channels,
    invite the bot to those channels first.

    Examples:
        slack search "deploy"
        slack search "kubernetes error" --channels eng-infra,eng-ai
        slack search "database migration" --from alice --depth 500
    """
    from .client import search_messages

    channel_list = [c.strip() for c in channels.split(",")] if channels else None
    results = search_messages(
        query,
        max_results=limit,
        channels=channel_list,
        from_user=from_user,
        messages_per_channel=depth,
    )

    if not results:
        console.print("[yellow]No messages found.[/]")
        raise typer.Exit()

    if full:
        for i, msg in enumerate(results, 1):
            console.print(f"\n[bold cyan]#{msg['channel']}[/] | [green]{msg['user']}[/]")
            console.print(msg["text"])
            console.print(f"[dim]{msg['permalink']}[/]")
            if i < len(results):
                console.print("---")
    else:
        table = Table(title=f"Slack: '{query}' ({len(results)} results)")
        table.add_column("Channel", style="cyan", max_width=15)
        table.add_column("User", style="green", max_width=15)
        table.add_column("Message", style="white", max_width=80)

        for msg in results:
            text = msg["text"][:80].replace("\n", " ")
            if len(msg["text"]) > 80:
                text += "..."
            table.add_row(f"#{msg['channel']}", msg["user"], text)

        console.print(table)


@app.command("channel-direct")
def channel_direct(
    name: str = typer.Argument(..., help="Slack channel ID, e.g. C1234567890"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max messages"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full message text"),
    cursor: str = typer.Option(None, "--cursor", help="Slack pagination cursor for the next page"),
    oldest: str = typer.Option(
        None,
        "--oldest",
        help="Oldest timestamp boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    latest: str = typer.Option(
        None,
        "--latest",
        help="Latest timestamp boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    inclusive: bool = typer.Option(
        False,
        "--inclusive",
        help="Include messages exactly on the oldest/latest boundary",
    ),
    allow_name_resolution: bool = typer.Option(
        False,
        "--allow-name-resolution",
        help="Allow resolving a channel name instead of requiring an explicit Slack channel ID",
    ),
    json_output: bool = typer.Option(False, "--json", help="Output full page metadata as JSON"),
):
    """Get recent messages from a channel directly with the Slack SDK."""
    import sys

    from .client import get_channel_history_page

    if not allow_name_resolution and not _channel_arg_is_id(name):
        stderr_console.print(
            "[red]Error: slack channel-direct requires an explicit Slack channel ID like C1234567890. "
            "Pass --allow-name-resolution to resolve a channel name intentionally.[/]"
        )
        raise typer.Exit(1)

    try:
        page = get_channel_history_page(
            name,
            limit=limit,
            cursor=cursor,
            oldest=oldest,
            latest=latest,
            inclusive=inclusive,
        )
    except (RuntimeError, ValueError) as e:
        stderr_console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1) from e

    messages = page["messages"]

    if not messages:
        console.print("[yellow]No messages found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(page, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    header = f"[bold]#{page['channel']}[/] - {len(messages)} messages"
    if page.get("has_more"):
        header += " [dim](more available)[/]"
    console.print(f"{header}\n")

    if page["window"]["oldest"] or page["window"]["latest"] or page.get("next_cursor"):
        console.print(
            f"[dim]window oldest={page['window']['oldest']} latest={page['window']['latest']} next_cursor={page.get('next_cursor')}[/]\n"
        )

    for msg in messages:
        text = msg["text"] if full else msg["text"][:120].replace("\n", " ")
        if not full and len(msg["text"]) > 120:
            text += "..."

        thread_info = f" [dim]({msg['reply_count']} replies)[/]" if msg.get("reply_count") else ""
        console.print(f"[green]{msg['user']}[/]{thread_info}: {text}")


@app.command("channel")
def channel(
    channel_id: str = typer.Argument(..., help="Slack channel ID, e.g. C1234567890"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max messages"),
    cursor: str = typer.Option(None, "--cursor", help="Slack pagination cursor for the next page"),
    oldest: str = typer.Option(None, "--oldest", help="Oldest Slack timestamp boundary"),
    latest: str = typer.Option(None, "--latest", help="Latest Slack timestamp boundary"),
    inclusive: bool | None = typer.Option(
        None,
        "--inclusive/--exclusive",
        help="Include messages exactly on the oldest/latest boundary",
    ),
    include_all_metadata: bool | None = typer.Option(
        None,
        "--include-all-metadata/--metadata-default",
        help="Ask Slack to return all message metadata",
    ),
    full: bool = typer.Option(False, "--full", "-f", help="Show full message text"),
    json_output: bool = typer.Option(False, "--json", help="Output raw proxy response as JSON"),
):
    """Get channel history through the Centaur API server proxy."""
    import sys

    from .client import get_channel_history_proxy

    try:
        page = get_channel_history_proxy(
            channel_id,
            cursor=cursor,
            include_all_metadata=include_all_metadata,
            inclusive=inclusive,
            latest=latest,
            limit=limit,
            oldest=oldest,
        )
    except (RuntimeError, ValueError) as e:
        stderr_console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1) from e

    if json_output:
        print(json.dumps(page, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    messages = page.get("messages", [])
    if not messages:
        console.print("[yellow]No messages found.[/]")
        raise typer.Exit()

    header = f"[bold]#{channel_id}[/] - {len(messages)} messages"
    if page.get("has_more"):
        header += " [dim](more available)[/]"
    console.print(f"{header}\n")

    next_cursor = page.get("response_metadata", {}).get("next_cursor")
    if next_cursor:
        console.print(f"[dim]next_cursor={next_cursor}[/]\n")

    for msg in messages:
        user = msg.get("user") or msg.get("bot_id") or msg.get("username") or "unknown"
        text = str(msg.get("text") or "")
        if not full:
            text = text[:120].replace("\n", " ")
            if len(str(msg.get("text") or "")) > 120:
                text += "..."
        thread_info = f" [dim]({msg['reply_count']} replies)[/]" if msg.get("reply_count") else ""
        console.print(f"[green]{user}[/]{thread_info}: {text}")


def _parse_thread_ref(permalink: str) -> tuple[str, str]:
    import re

    if permalink.startswith("https://"):
        match = re.search(r"/archives/([A-Z0-9]+)/p(\d+)", permalink)
        if not match:
            console.print("[red]Invalid permalink format[/]")
            raise typer.Exit(1)
        channel_id = match.group(1)
        ts_raw = match.group(2)
        return channel_id, f"{ts_raw[:10]}.{ts_raw[10:]}"
    if ":" in permalink:
        channel_id, thread_ts = permalink.split(":", 1)
        return channel_id, thread_ts
    console.print("[red]Provide a Slack permalink or 'channel_id:timestamp'[/]")
    raise typer.Exit(1)


@app.command("thread")
def thread(
    permalink: str = typer.Argument(..., help="Slack permalink or 'channel_id:timestamp'"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max messages to return from the thread"),
    cursor: str = typer.Option(None, "--cursor", help="Slack pagination cursor for the next page"),
    oldest: str = typer.Option(
        None,
        "--oldest",
        help="Oldest timestamp boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    latest: str = typer.Option(
        None,
        "--latest",
        help="Latest timestamp boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    inclusive: bool = typer.Option(
        True, "--inclusive/--exclusive", help="Include the boundary timestamps"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get all replies in a thread.

    Examples:
        slack thread "https://slack.com/archives/C01234567/p1234567890123456"
        slack thread "C01234567:1234567890.123456"
        slack thread "https://..." --json
    """
    import sys

    from .client import get_thread_replies_page, get_thread_replies_proxy

    channel_id, thread_ts = _parse_thread_ref(permalink)

    try:
        page = get_thread_replies_proxy(
            channel_id,
            thread_ts,
            limit=limit,
            cursor=cursor,
            oldest=oldest,
            latest=latest,
            inclusive=inclusive,
        )
    except (RuntimeError, ValueError):
        try:
            page = get_thread_replies_page(
                channel_id,
                thread_ts,
                limit=limit,
                cursor=cursor,
                oldest=oldest,
                latest=latest,
                inclusive=inclusive,
            )
        except (RuntimeError, ValueError) as direct_error:
            stderr_console.print(f"[red]Error: {direct_error}[/]")
            raise typer.Exit(1) from direct_error

    messages = page.get("messages", [])

    if not messages:
        console.print("[yellow]No messages found in thread.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(page, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    header = f"\n[bold]Thread ({len(messages)} messages)[/]"
    if page.get("has_more"):
        header += " [dim](more available)[/]"
    console.print(f"{header}\n")

    next_cursor = page.get("response_metadata", {}).get("next_cursor")
    if next_cursor:
        console.print(f"[dim]next_cursor={next_cursor}[/]\n")

    for i, msg in enumerate(messages):
        prefix = "[bold]>[/]" if i == 0 else "  "
        user = msg.get("user") or msg.get("bot_id") or msg.get("username") or "unknown"
        text = str(msg.get("text") or "").replace("\n", "\n     ")
        console.print(f"{prefix} [cyan]@{user}[/]: {text}\n")


@app.command("thread-direct")
def thread_direct(
    permalink: str = typer.Argument(..., help="Slack permalink or 'channel_id:timestamp'"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max messages to return from the thread"),
    cursor: str = typer.Option(None, "--cursor", help="Slack pagination cursor for the next page"),
    oldest: str = typer.Option(
        None,
        "--oldest",
        help="Oldest timestamp boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    latest: str = typer.Option(
        None,
        "--latest",
        help="Latest timestamp boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    inclusive: bool = typer.Option(
        True, "--inclusive/--exclusive", help="Include the boundary timestamps"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get all replies in a thread directly with the Slack SDK.

    Examples:
        slack thread-direct "https://slack.com/archives/C01234567/p1234567890123456"
        slack thread-direct "C01234567:1234567890.123456"
        slack thread-direct "https://..." --json
    """
    import sys

    from .client import get_thread_replies_page

    channel_id, thread_ts = _parse_thread_ref(permalink)

    try:
        page = get_thread_replies_page(
            channel_id,
            thread_ts,
            limit=limit,
            cursor=cursor,
            oldest=oldest,
            latest=latest,
            inclusive=inclusive,
        )
    except (RuntimeError, ValueError) as e:
        stderr_console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1) from e

    messages = page["messages"]

    if not messages:
        console.print("[yellow]No messages found in thread.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(page, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    header = f"\n[bold]Thread ({len(messages)} messages)[/]"
    if page.get("has_more"):
        header += " [dim](more available)[/]"
    console.print(f"{header}\n")

    if page["window"]["oldest"] or page["window"]["latest"] or page.get("next_cursor"):
        console.print(
            f"[dim]window oldest={page['window']['oldest']} latest={page['window']['latest']} next_cursor={page.get('next_cursor')}[/]\n"
        )

    for i, msg in enumerate(messages):
        prefix = "[bold]>[/]" if i == 0 else "  "
        user = f"[cyan]@{msg['user']}[/]"
        text = msg["text"].replace("\n", "\n     ")
        console.print(f"{prefix} {user}: {text}\n")


@app.command("sync-history")
def sync_history(
    channel: str = typer.Argument(..., help="Channel name (without #) or channel ID"),
    limit: int = typer.Option(200, "--limit", "-n", help="Max messages to fetch this run"),
    lookback_days: int = typer.Option(
        30,
        "--lookback-days",
        help="Re-read this trailing window on each sync to catch edits and deletes",
    ),
    state_file: str = typer.Option(
        None,
        "--state-file",
        help="JSON file containing prior sync state; updated in place on success",
    ),
    oldest: str = typer.Option(
        None,
        "--oldest",
        help="Override the oldest boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    latest: str = typer.Option(
        None,
        "--latest",
        help="Override the latest boundary: Slack ts, epoch, ISO datetime, or YYYY-MM-DD",
    ),
    json_output: bool = typer.Option(False, "--json", help="Output the sync payload as JSON"),
):
    """Run an incremental channel-history sync suitable for ETL jobs."""
    from pathlib import Path
    import sys

    from .client import sync_channel_history

    state = None
    state_path = Path(state_file) if state_file else None
    if state_path and state_path.exists():
        state = json.loads(state_path.read_text())

    try:
        result = sync_channel_history(
            channel,
            state=state,
            limit=limit,
            lookback_days=lookback_days,
            oldest=oldest,
            latest=latest,
        )
    except (RuntimeError, ValueError) as e:
        stderr_console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if state_path:
        state_path.parent.mkdir(parents=True, exist_ok=True)
        state_path.write_text(json.dumps(result["sync_state"], indent=2, ensure_ascii=False))

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(
        f"[bold]#{result['channel']}[/] fetched {result['count']} messages"
        + (" [dim](more available)[/]" if result.get("has_more") else "")
    )
    console.print(f"[dim]next_cursor={result.get('next_cursor')}[/]")
    console.print(f"[dim]sync_state={json.dumps(result['sync_state'], ensure_ascii=False)}[/]")


def _render_channels(results: list[dict], title: str, include_access: bool = False) -> None:
    """Render a Slack channel list."""
    if not results:
        console.print("[yellow]No channels found.[/]")
        raise typer.Exit()

    table = Table(title=title)
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Members", style="green", justify="right", max_width=8)
    if include_access:
        table.add_column("Access", style="magenta", max_width=12)
    table.add_column("Purpose", style="white", max_width=50)

    for ch in results:
        priv = "[dim]🔒[/]" if ch["is_private"] else ""
        purpose = (ch["purpose"] or ch["topic"] or "")[:50]
        row = [f"#{ch['name']}{priv}", str(ch["member_count"])]
        if include_access:
            access = "".join(
                label
                for label, enabled in [
                    ("h", ch.get("can_read_history")),
                    ("u", ch.get("can_upload")),
                    ("d", ch.get("can_download")),
                ]
                if enabled
            )
            row.append(access or "-")
        row.append(purpose)
        table.add_row(*row)

    console.print(table)


@app.command("channels")
def channels(
    limit: int = typer.Option(100, "--limit", "-n", help="Max channels"),
    query: str = typer.Option(None, "--query", "-q", help="Filter by name"),
    bot_member_only: bool = typer.Option(
        False,
        "--bot-member-only",
        help="Only list JWT-authorized channels with history access",
    ),
):
    """List Slack channels authorized by the Centaur API server proxy JWT."""
    from .client import list_channels_proxy

    results = list_channels_proxy(limit=limit, history_only=bot_member_only)

    if query:
        results = [c for c in results if query.lower() in c["name"].lower()]

    _render_channels(results, f"Channels ({len(results)})", include_access=True)


@app.command("channels-direct")
def channels_direct(
    limit: int = typer.Option(100, "--limit", "-n", help="Max channels"),
    query: str = typer.Option(None, "--query", "-q", help="Filter by name"),
    bot_member_only: bool = typer.Option(
        False,
        "--bot-member-only",
        help="Only list channels the bot can actually read history from",
    ),
):
    """List Slack channels directly with the Slack SDK."""
    from .client import list_bot_channels, list_channels

    if bot_member_only:
        results = list_bot_channels(limit=limit)
    else:
        results = list_channels(limit=limit)

    if query:
        results = [c for c in results if query.lower() in c["name"].lower()]

    _render_channels(results, f"Channels ({len(results)})")


def _render_channel_members(channel: str, members: list[dict], emails_only: bool) -> None:
    if not members:
        console.print("[yellow]No members found.[/]")
        raise typer.Exit()

    if emails_only:
        for m in members:
            if m.get("email"):
                console.print(m["email"])
        return

    table = Table(title=f"#{channel} Members ({len(members)})")
    table.add_column("Name", style="cyan", max_width=20)
    table.add_column("Real Name", style="white", max_width=25)
    table.add_column("Email", style="green", max_width=35)

    for m in members:
        table.add_row(f"@{m['name']}", m.get("real_name", ""), m.get("email", ""))

    console.print(table)


@app.command("channel-members-proxy")
@app.command("channel-members")
def channel_members_cmd(
    channel_id: str = typer.Argument(..., help="Slack channel ID, e.g. C1234567890"),
    limit: int = typer.Option(1000, "--limit", "-n", help="Max members"),
    emails_only: bool = typer.Option(
        False, "--emails", "-e", help="Output only email addresses (one per line)"
    ),
):
    """List members of a Slack channel through the Centaur API server proxy.

    Examples:
        slack channel-members C1234567890
        slack channel-members C1234567890 --emails
    """
    from .client import get_channel_members_proxy

    try:
        members = get_channel_members_proxy(channel_id, limit=limit)
    except (RuntimeError, ValueError) as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    _render_channel_members(channel_id, members, emails_only)


@app.command("channel-members-direct")
def channel_members_direct_cmd(
    channel: str = typer.Argument(..., help="Channel name (without #) or channel ID"),
    emails_only: bool = typer.Option(
        False, "--emails", "-e", help="Output only email addresses (one per line)"
    ),
):
    """List all members of a Slack channel directly with the Slack SDK.

    Examples:
        slack channel-members-direct eng-ai
        slack channel-members-direct eng-ai --emails
    """
    from .client import get_channel_members

    try:
        members = get_channel_members(channel)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    _render_channel_members(channel, members, emails_only)


@app.command()
def users(
    limit: int = typer.Option(100, "--limit", "-n", help="Max users"),
    query: str = typer.Option(None, "--query", "-q", help="Filter by name/email"),
    bots: bool = typer.Option(False, "--bots", "-b", help="Include bots"),
):
    """List all Slack workspace members."""
    from .client import list_users

    results = list_users(limit=limit)

    if not bots:
        results = [u for u in results if not u["is_bot"]]

    if query:
        query_lower = query.lower()
        results = [
            u
            for u in results
            if query_lower in u["name"].lower()
            or query_lower in u["real_name"].lower()
            or query_lower in u["email"].lower()
        ]

    if not results:
        console.print("[yellow]No users found.[/]")
        raise typer.Exit()

    table = Table(title=f"Users ({len(results)})")
    table.add_column("Name", style="cyan", max_width=20)
    table.add_column("Real Name", style="white", max_width=25)
    table.add_column("Title", style="dim", max_width=30)

    for u in results:
        bot = " [dim]🤖[/]" if u["is_bot"] else ""
        table.add_row(f"@{u['name']}{bot}", u["real_name"], u["title"][:30])

    console.print(table)


@app.command("upload-direct")
def upload_direct(
    channel: str = typer.Argument(
        ..., help="Slack channel/conversation ID to upload into, e.g. C123 or D123"
    ),
    files: list[str] = typer.Argument(..., help="File path(s) to upload"),  # noqa: B008
    comment: str = typer.Option(None, "--comment", "-c", help="Comment to post with files"),
    thread: str = typer.Option(..., "--thread", "-t", help="Slack thread timestamp to reply to"),
):
    """Upload file(s) directly with the Slack SDK.

    Examples:
        slack upload-direct C123 screenshot.png --thread 1234567890.123456
        slack upload-direct C123 file1.png file2.jpg --thread 1234567890.123456 -c "Here are the files"
    """
    import base64
    from pathlib import Path

    from .client import upload_file

    if not _channel_arg_is_id(channel):
        console.print(
            "[red]Error: upload-direct channel must be a Slack conversation ID like C123 or D123[/]"
        )
        raise typer.Exit(1)

    first_upload_path = files[0] if files else None

    for file_path in files:
        path = Path(file_path)
        if not path.exists():
            console.print(f"[red]File not found: {file_path}[/]")
            raise typer.Exit(1)

        try:
            # Read the file locally and hand the bytes to the tool: upload_file
            # takes no local path (it runs server-side; see client.py).
            result = upload_file(
                channel=channel,
                content_base64=base64.b64encode(path.read_bytes()).decode(),
                filename=path.name,
                title=path.name,
                comment=comment
                if file_path == first_upload_path
                else None,  # Only comment on first file
                thread_ts=thread,
            )
            console.print(f"[green]✓ Uploaded {path.name}[/]")
            console.print(f"[dim]{result['permalink']}[/]")
        except (RuntimeError, ValueError) as e:
            console.print(f"[red]Error uploading {path.name}: {e}[/]")
            raise typer.Exit(1) from e


@app.command("upload-proxy")
@app.command("upload")
def upload(
    channel_id: str = typer.Argument(
        ..., help="Slack channel/conversation ID to upload into, e.g. C123 or D123"
    ),
    files: list[str] = typer.Argument(..., help="File path(s) to upload"),  # noqa: B008
    comment: str = typer.Option(None, "--comment", "-c", help="Comment to post with files"),
    thread: str = typer.Option(None, "--thread", "-t", help="Slack thread timestamp to reply to"),
    content_type: str = typer.Option(
        None, "--content-type", help="Content-Type to send for all files"
    ),
    alt_text: str = typer.Option(None, "--alt-text", help="Alt text for all files"),
    snippet_type: str = typer.Option(None, "--snippet-type", help="Slack snippet type"),
):
    """Upload file(s) through the Centaur API server Slack proxy."""
    import base64
    import mimetypes
    from pathlib import Path

    from .client import upload_file_proxy

    if not _channel_arg_is_id(channel_id):
        console.print(
            "[red]Error: upload channel must be a Slack conversation ID like C123 or D123[/]"
        )
        raise typer.Exit(1)

    first_upload_path = files[0] if files else None
    for file_path in files:
        path = Path(file_path)
        if not path.exists():
            console.print(f"[red]File not found: {file_path}[/]")
            raise typer.Exit(1)

        effective_content_type = content_type or mimetypes.guess_type(path.name)[0]
        try:
            result = upload_file_proxy(
                channel_id=channel_id,
                content_base64=base64.b64encode(path.read_bytes()).decode(),
                filename=path.name,
                title=path.name,
                initial_comment=comment if file_path == first_upload_path else None,
                thread_ts=thread,
                content_type=effective_content_type,
                alt_txt=alt_text,
                snippet_type=snippet_type,
            )
            console.print(f"[green]✓ Uploaded {path.name}[/]")
            console.print(
                f"[dim]{result.get('file_id') or result.get('file', {}).get('id', '')}[/]"
            )
        except (RuntimeError, ValueError) as e:
            console.print(f"[red]Error uploading {path.name}: {e}[/]")
            raise typer.Exit(1) from e


@app.command()
def questions(
    channel: str = typer.Argument(..., help="Channel name (without #)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Messages to scan"),
):
    """Find questions in a channel (messages ending with ? or containing question words)."""
    from .client import get_channel_history

    messages = get_channel_history(channel, limit=limit)

    question_words = [
        "how",
        "why",
        "what",
        "when",
        "where",
        "who",
        "which",
        "can i",
        "could",
        "should",
        "is there",
        "does anyone",
        "has anyone",
    ]

    questions = []
    for msg in messages:
        text = msg["text"].lower()
        is_question = text.rstrip().endswith("?") or any(text.startswith(w) for w in question_words)
        if is_question and len(msg["text"]) > 10:
            questions.append(msg)

    if not questions:
        console.print("[yellow]No questions found.[/]")
        raise typer.Exit()

    console.print(f"[bold]#{channel}[/] - {len(questions)} questions found\n")

    for msg in questions:
        text = msg["text"][:150].replace("\n", " ")
        if len(msg["text"]) > 150:
            text += "..."
        replies = f" ({msg['reply_count']} replies)" if msg.get("reply_count") else ""
        console.print(f"[green]{msg['user']}[/]{replies}: {text}\n")


@app.command()
def usergroups(
    query: str = typer.Option(None, "--query", "-q", help="Filter by handle/name"),
):
    """List all Slack user groups."""
    from .client import list_usergroups

    results = list_usergroups()

    if query:
        query_lower = query.lower()
        results = [
            g
            for g in results
            if query_lower in g["handle"].lower() or query_lower in g["name"].lower()
        ]

    if not results:
        console.print("[yellow]No user groups found.[/]")
        raise typer.Exit()

    table = Table(title=f"User Groups ({len(results)})")
    table.add_column("Handle", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Members", style="green", justify="right", max_width=8)
    table.add_column("Description", style="dim", max_width=40)

    for g in results:
        table.add_row(f"@{g['handle']}", g["name"], str(g["user_count"]), g["description"][:40])

    console.print(table)


@app.command()
def usergroup_create(
    handle: str = typer.Argument(..., help="Handle for the group (e.g., 'perf')"),
    name: str = typer.Argument(..., help="Display name for the group"),
    description: str = typer.Option("", "--description", "-d", help="Group description"),
    users: str = typer.Option(None, "--users", "-u", help="Comma-separated user IDs to add"),
):
    """Create a new user group.

    Examples:
        slack usergroup-create perf "Performance Team"
        slack usergroup-create perf "Performance Team" -u U123,U456,U789
    """
    from .client import create_usergroup

    user_ids = users.split(",") if users else None

    try:
        result = create_usergroup(handle, name, description, user_ids)
        console.print(f"[green]✓ Created @{result['handle']}[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def usergroup_update(
    handle: str = typer.Argument(..., help="Group handle (e.g., 'perf')"),
    users: str = typer.Argument(..., help="Comma-separated user IDs to set as members"),
):
    """Update members of a user group.

    Examples:
        slack usergroup-update perf U123,U456,U789
    """
    from .client import update_usergroup_users

    user_ids = [u.strip() for u in users.split(",")]

    try:
        result = update_usergroup_users(handle, user_ids)
        console.print(f"[green]✓ Updated @{result['handle']} with {len(user_ids)} members[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("search-files")
def search_files_cmd(
    channel_id: str = typer.Argument(..., help="Slack channel ID to search"),
    query: str = typer.Argument(..., help="Search query for files"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
):
    """Search files shared in a Slack channel.

    Examples:
        slack search-files C123456789 "quarterly report"
        slack search-files C123456789 "architecture diagram" -n 10
    """
    from .client import search_files

    try:
        results = search_files(channel_id, query, max_results=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    _print_file_search_results(query, results)


@app.command("search-files-direct")
def search_files_direct_cmd(
    query: str = typer.Argument(..., help="Search query for files"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
):
    """Search files by calling Slack files.list directly.

    Examples:
        slack search-files-direct "quarterly report"
        slack search-files-direct "architecture diagram" -n 10
    """
    from .client import search_files_direct

    try:
        results = search_files_direct(query, max_results=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    _print_file_search_results(query, results)


def _format_file_size(size: object) -> str:
    try:
        size_bytes = int(size or 0)
    except (TypeError, ValueError):
        return str(size)
    if size_bytes >= 1_000_000:
        return f"{size_bytes / 1_000_000:.1f}MB"
    if size_bytes >= 1_000:
        return f"{size_bytes / 1_000:.0f}KB"
    return f"{size_bytes}B"


def _format_file_info_value(key: str, value: object) -> str:
    if key == "size":
        return _format_file_size(value)
    return str(value)


def _print_file_search_results(query: str, results: list[dict]) -> None:
    if not results:
        console.print("[yellow]No files found.[/]")
        raise typer.Exit()

    table = Table(title=f"Files matching '{query}' ({len(results)})")
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("Type", style="dim", max_width=8)
    table.add_column("User", style="green", max_width=15)
    table.add_column("Size", style="dim", justify="right", max_width=10)

    for f in results:
        table.add_row(f["name"], f["filetype"], f["user"], _format_file_size(f["size"]))

    console.print(table)


@app.command("file-info-proxy")
@app.command("file-info")
def file_info(
    file_id: str = typer.Argument(..., help="Slack file ID, e.g. F1234567890"),
    channel_id: str = typer.Argument(
        ..., help="Slack channel/conversation ID that the file is shared in"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw metadata as JSON"),
):
    """Fetch Slack file metadata through the Centaur API server Slack proxy."""
    import sys

    from .client import file_info_proxy

    try:
        result = file_info_proxy(file_id=file_id, channel_id=channel_id)
    except (RuntimeError, ValueError) as e:
        console.print(f"[red]Error fetching Slack file info: {e}[/]")
        raise typer.Exit(1) from e

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    file = result.get("file", {})
    table = Table(title=f"Slack File {result.get('file_id') or file_id}")
    table.add_column("Field", style="cyan", max_width=18)
    table.add_column("Value", style="white", max_width=90)
    for key in [
        "id",
        "name",
        "title",
        "mimetype",
        "filetype",
        "size",
        "user",
        "created",
        "permalink",
        "url_private",
    ]:
        value = file.get(key)
        if value not in (None, "", []):
            table.add_row(key, _format_file_info_value(key, value))
    console.print(table)


@app.command("search-users")
def search_users_cmd(
    query: str = typer.Argument(..., help="Search by name, email, or title"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
):
    """Search workspace users by name, email, or title.

    Examples:
        slack search-users georgios
        slack search-users "@paradigm.xyz"
        slack search-users "engineer"
    """
    from .client import search_users

    try:
        results = search_users(query, max_results=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if not results:
        console.print("[yellow]No users found.[/]")
        raise typer.Exit()

    table = Table(title=f"Users matching '{query}' ({len(results)})")
    table.add_column("Name", style="cyan", max_width=20)
    table.add_column("Real Name", style="white", max_width=25)
    table.add_column("Email", style="green", max_width=35)
    table.add_column("Title", style="dim", max_width=30)

    for u in results:
        table.add_row(f"@{u['name']}", u["real_name"], u["email"], u["title"][:30])

    console.print(table)


@app.command()
def dump(
    name: str = typer.Argument(..., help="Channel name (without #)"),
    output: str = typer.Option(None, "--output", "-o", help="Output file path (default: stdout)"),
    limit: int = typer.Option(500, "--limit", "-n", help="Max messages from channel"),
    min_replies: int = typer.Option(
        0, "--min-replies", "-r", help="Only include threads with >= N replies"
    ),
):
    """Dump full channel history with all thread replies to JSON.

    Fetches channel messages and expands all threads inline. Useful for
    analyzing conversations and finding multi-turn interactions.

    Examples:
        slack dump test-bot -o /tmp/test-bot.json
        slack dump test-bot --min-replies 3  # Only threads with 3+ replies
        slack dump test-bot -n 100 | jq '.[] | select(.replies | length > 5)'
    """
    import json
    import sys

    from .client import dump_channel_with_threads

    try:
        data = dump_channel_with_threads(name, limit=limit, min_replies=min_replies)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]", file=sys.stderr)
        raise typer.Exit(1)

    result = json.dumps(data, indent=2, ensure_ascii=False)

    if output:
        from pathlib import Path

        Path(output).write_text(result)
        import sys as _sys

        _sys.stderr.write(f"✓ Dumped {len(data['messages'])} messages to {output}\n")
        _sys.stderr.write(
            f"  {data['stats']['threads_fetched']} threads expanded, {data['stats']['total_replies']} total replies\n"
        )
    else:
        print(result)


@app.command()
def files(
    permalink: str = typer.Argument(
        ..., help="Slack message permalink, channel:timestamp, or url_private"
    ),
    download: bool = typer.Option(
        False, "--download", "-d", help="Download files to current directory"
    ),
    output: str = typer.Option(".", "--output", "-o", help="Output directory for downloads"),
):
    """List or download files attached to a message.

    Examples:
        slack files "https://slack.com/archives/C01234567/p1234567890123456"
        slack files "https://files.slack.com/files-pri/T1-F1/report.pdf" --download
        slack files "https://..." --download
        slack files "https://..." -d -o /tmp/slack-files
    """
    import re
    from urllib.parse import urlparse

    from .client import get_message_files

    parsed = urlparse(permalink)
    if parsed.scheme == "https" and (parsed.hostname or "").lower() == "files.slack.com":
        if not download:
            console.print("[red]Pass --download to download a direct Slack file URL[/]")
            raise typer.Exit(1)
        _download_direct_url(permalink, output)
        return

    if permalink.startswith("https://"):
        match = re.search(r"/archives/([A-Z0-9]+)/p(\d+)", permalink)
        if not match:
            console.print("[red]Invalid permalink format[/]")
            raise typer.Exit(1)
        channel_id = match.group(1)
        ts_raw = match.group(2)
        message_ts = f"{ts_raw[:10]}.{ts_raw[10:]}"
    elif ":" in permalink:
        channel_id, message_ts = permalink.split(":", 1)
    else:
        console.print("[red]Provide a Slack permalink or 'channel_id:timestamp'[/]")
        raise typer.Exit(1)

    files_list = get_message_files(channel_id, message_ts)

    if not files_list:
        console.print("[yellow]No files attached to this message.[/]")
        raise typer.Exit()

    if download:
        for f in files_list:
            if not f["url_private"]:
                console.print(f"[yellow]⚠ No download URL for {f['name']}[/]")
                continue

            _download_direct_url(f["url_private"], output, display_name=f["name"])
    else:
        console.print(f"[bold]Files ({len(files_list)})[/]\n")
        for f in files_list:
            console.print(
                f"[cyan]{f['name']}[/] ({f['filetype']}, {_format_file_size(f['size'])})"
            )
            console.print(f"  [dim]{f['url_private']}[/]")


def _download_direct_url(url: str, output: str, display_name: str | None = None) -> None:
    from pathlib import Path

    from .client import _fetch_slack_file

    output_dir = Path(output)
    output_dir.mkdir(parents=True, exist_ok=True)
    try:
        filename, _mime_type, body = _fetch_slack_file(url)
        out_path = output_dir / (display_name or filename)
        out_path.write_bytes(body)
        console.print(f"[green]✓ Downloaded {out_path.name}[/] ({len(body)} bytes)")
        console.print(f"[dim]{out_path.absolute()}[/]")
    except Exception as e:
        console.print(f"[red]Error downloading Slack file: {e}[/]")
        raise typer.Exit(1) from e


@app.command("download-direct")
def download_direct(
    permalink: str = typer.Argument(
        ..., help="Slack message permalink, channel:timestamp, or url_private"
    ),
    output: str = typer.Option(".", "--output", "-o", help="Output directory for downloads"),
):
    """Download Slack files directly with the Slack bot token."""
    _download_direct(permalink, output)


def _download_direct(permalink: str, output: str) -> None:
    import re
    from urllib.parse import urlparse

    from .client import get_message_files

    parsed = urlparse(permalink)
    if parsed.scheme == "https" and (parsed.hostname or "").lower() == "files.slack.com":
        _download_direct_url(permalink, output)
        return

    if permalink.startswith("https://"):
        match = re.search(r"/archives/([A-Z0-9]+)/p(\d+)", permalink)
        if not match:
            console.print("[red]Invalid permalink format[/]")
            raise typer.Exit(1)
        channel_id = match.group(1)
        ts_raw = match.group(2)
        message_ts = f"{ts_raw[:10]}.{ts_raw[10:]}"
    elif ":" in permalink:
        channel_id, message_ts = permalink.split(":", 1)
    else:
        console.print("[red]Provide a Slack permalink, 'channel_id:timestamp', or url_private[/]")
        raise typer.Exit(1)

    files_list = get_message_files(channel_id, message_ts)
    if not files_list:
        console.print("[yellow]No files attached to this message.[/]")
        raise typer.Exit()

    for f in files_list:
        if not f["url_private"]:
            console.print(f"[yellow]⚠ No download URL for {f['name']}[/]")
            continue
        _download_direct_url(f["url_private"], output, display_name=f["name"])


@app.command("download-proxy")
@app.command("download")
def download(
    file_id: str = typer.Argument(..., help="Slack file ID, e.g. F1234567890"),
    channel_id: str = typer.Argument(
        ..., help="Slack channel/conversation ID that the file is shared in"
    ),
    output: str = typer.Option(".", "--output", "-o", help="Output directory for downloads"),
    json_output: bool = typer.Option(False, "--json", help="Print metadata as JSON"),
):
    """Download a Slack file through the Centaur API server Slack proxy."""
    import base64
    import sys
    from pathlib import Path

    from .client import download_file_proxy

    try:
        result = download_file_proxy(file_id=file_id, channel_id=channel_id)
    except (RuntimeError, ValueError) as e:
        console.print(f"[red]Error downloading Slack file: {e}[/]")
        raise typer.Exit(1) from e

    if json_output:
        metadata = {key: value for key, value in result.items() if key != "content_base64"}
        print(json.dumps(metadata, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    output_dir = Path(output)
    output_dir.mkdir(parents=True, exist_ok=True)
    out_path = output_dir / result["filename"]
    out_path.write_bytes(base64.b64decode(result["content_base64"]))
    console.print(f"[green]✓ Downloaded {result['filename']}[/] ({result['size_bytes']} bytes)")
    console.print(f"[dim]{out_path.absolute()}[/]")


# === Feedback Commands ===


@app.command()
def feedback(
    action: str = typer.Argument(
        "collect",
        help="Action: collect, backfill, digest, show, update-status, improve, loop",
    ),
    channels: str = typer.Option(
        "test-bot",
        "--channels",
        "-c",
        help="Comma-separated channel names to scan",
    ),
    since_days: int = typer.Option(
        None,
        "--since-days",
        "-d",
        help="Override checkpoint, scan last N days",
    ),
    limit: int = typer.Option(200, "--limit", "-n", help="Max threads per channel"),
    status: str = typer.Option(
        None, "--status", "-s", help="Filter by status (new, triaged, fixed)"
    ),
    category: str = typer.Option(None, "--category", help="Filter by category"),
    severity: str = typer.Option(None, "--severity", help="Min severity (low, medium, high)"),
    item_id: int = typer.Option(None, "--id", help="Feedback item ID (for show/update-status)"),
    new_status: str = typer.Option(None, "--new-status", help="New status for update-status"),
    output: str = typer.Option(None, "--output", "-o", help="Output file path"),
    max_items: int = typer.Option(
        8, "--max-items", help="Max actionable feedback items per improvement run"
    ),
    persona: str = typer.Option(
        "eng", "--persona", help="Persona to use for auto-improvement runs"
    ),
    harness: str = typer.Option(
        "amp", "--harness", help="Harness to use for auto-improvement runs"
    ),
    interval_sec: int = typer.Option(
        900, "--interval-sec", help="Sleep interval between loop iterations"
    ),
    iterations: int = typer.Option(
        0, "--iterations", help="Number of loop iterations to run; 0 means forever"
    ),
    dry_run: bool = typer.Option(
        False, "--dry-run", help="Build the improvement prompt without dispatching an agent run"
    ),
):
    """Collect and analyze feedback from bot interactions.

    Actions:
      collect  - Scan channels for new feedback (incremental)
      backfill - Scan historical feedback ignoring the usual per-channel cap
      digest   - Generate markdown digest of feedback
      show     - Show details of a specific feedback item
      update-status - Update status of a feedback item
      improve  - Collect feedback and dispatch a background agent improvement run
      loop     - Repeatedly run the improvement cycle

    Examples:
        slack feedback collect -c test-bot
        slack feedback backfill -c test-bot --since-days 30 --limit 0
        slack feedback collect -c test-bot,eng-ai --since-days 7
        slack feedback digest --severity medium
        slack feedback digest --status new -o /tmp/digest.md
        slack feedback update-status --id 42 --new-status triaged
        slack feedback improve --persona eng --harness amp
        slack feedback loop --iterations 1 --interval-sec 300
    """
    import json
    import time

    from .feedback import (
        backfill_feedback,
        collect_feedback,
        format_digest_markdown,
        get_feedback_digest,
        init_db,
        run_improvement_cycle,
        update_feedback_status,
    )

    channel_list = [c.strip() for c in channels.split(",")]
    limit_per_channel = None if limit <= 0 else limit

    if action == "collect":
        console.print(f"[bold]Collecting feedback from: {', '.join(channel_list)}[/]")
        stats = collect_feedback(
            channels=channel_list,
            limit_per_channel=limit_per_channel,
            since_days=since_days,
        )
        console.print("\n[green]✓ Collection complete[/]")
        console.print(f"  Channels scanned: {stats['channels_scanned']}")
        console.print(f"  Threads analyzed: {stats['threads_analyzed']}")
        console.print(f"  Feedback items created: {stats['feedback_items_created']}")
        console.print(f"  Feedback items updated: {stats['feedback_items_updated']}")
        if stats["by_category"]:
            console.print(f"  By category: {stats['by_category']}")
        if stats["by_severity"]:
            console.print(f"  By severity: {stats['by_severity']}")

    elif action == "backfill":
        lookback_days = since_days or 30
        backfill_limit = None if limit == 200 else limit_per_channel
        console.print(f"[bold]Backfilling feedback from: {', '.join(channel_list)}[/]")
        stats = backfill_feedback(
            channels=channel_list,
            since_days=lookback_days,
            limit_per_channel=backfill_limit,
        )
        console.print("\n[green]✓ Backfill complete[/]")
        console.print(f"  Lookback days: {lookback_days}")
        console.print(f"  Channels scanned: {stats['channels_scanned']}")
        console.print(f"  Threads analyzed: {stats['threads_analyzed']}")
        console.print(f"  Feedback items created: {stats['feedback_items_created']}")
        console.print(f"  Feedback items updated: {stats['feedback_items_updated']}")
        if stats["by_category"]:
            console.print(f"  By category: {stats['by_category']}")
        if stats["by_severity"]:
            console.print(f"  By severity: {stats['by_severity']}")

    elif action == "digest":
        items = get_feedback_digest(
            since_days=since_days or 7,
            status=status,
            category=category,
            min_severity=severity,
        )
        md = format_digest_markdown(items)

        if output:
            from pathlib import Path

            Path(output).write_text(md)
            console.print(f"[green]✓ Digest written to {output}[/]")
        else:
            print(md)

    elif action == "show":
        if not item_id:
            console.print("[red]Error: --id required for show action[/]")
            raise typer.Exit(1)

        conn = init_db()
        row = conn.execute("SELECT * FROM feedback_items WHERE id = ?", (item_id,)).fetchone()
        conn.close()

        if not row:
            console.print(f"[red]Error: Feedback item {item_id} not found[/]")
            raise typer.Exit(1)

        console.print(f"\n[bold]Feedback Item #{row['id']}[/]\n")
        console.print(f"[cyan]Channel:[/] {row['slack_channel']}")
        console.print(f"[cyan]Permalink:[/] {row['permalink']}")
        console.print(f"[cyan]Category:[/] {row['category']}")
        console.print(f"[cyan]Severity:[/] {row['severity']}")
        console.print(f"[cyan]Status:[/] {row['status']}")
        console.print(f"[cyan]Reporter:[/] {row['reporter_user']}")
        console.print(f"[cyan]CLI:[/] {row['cli_involved'] or 'none'}")
        if row["amp_thread_id"]:
            console.print(
                f"[cyan]Amp Thread:[/] https://ampcode.com/threads/{row['amp_thread_id']}"
            )
        console.print(f"\n[cyan]Summary:[/]\n{row['summary']}")
        console.print(f"\n[cyan]Evidence:[/]\n{json.dumps(json.loads(row['evidence']), indent=2)}")

    elif action == "update-status":
        if not item_id or not new_status:
            console.print("[red]Error: --id and --new-status required[/]")
            raise typer.Exit(1)

        valid_statuses = ["new", "triaged", "in_progress", "fixed", "wontfix"]
        if new_status not in valid_statuses:
            console.print(f"[red]Error: Status must be one of: {valid_statuses}[/]")
            raise typer.Exit(1)

        if update_feedback_status(item_id, new_status):
            console.print(f"[green]✓ Updated item {item_id} to status: {new_status}[/]")
        else:
            console.print(f"[red]Error: Item {item_id} not found[/]")
            raise typer.Exit(1)

    elif action == "improve":
        console.print("[bold]Running auto-improvement cycle...[/]\n")
        result = run_improvement_cycle(
            channels=channel_list,
            since_days=since_days or 7,
            limit_per_channel=limit_per_channel,
            max_items=max_items,
            min_severity=severity or "medium",
            harness=harness,
            persona_id=persona,
            dry_run=dry_run,
        )

        collect_stats = result["collect_stats"]
        console.print(
            f"[dim]Collected: +{collect_stats['feedback_items_created']} new, {collect_stats['feedback_items_updated']} updated[/]"
        )

        if result["actionable_items"] == 0:
            console.print("\n[green]✓ No actionable feedback found![/]")
            console.print("[dim]All recent interactions were successful or low severity.[/]")
            return

        console.print(f"[cyan]Actionable items:[/] {result['actionable_items']}")
        console.print(f"[cyan]Item IDs:[/] {result['item_ids']}")
        if dry_run:
            print(result["prompt"])
            return

        console.print("\n[green]✓ Improvement agent dispatched[/]")
        console.print(f"  Thread key: {result['thread_key']}")
        console.print(f"  Execution id: {result['execution_id']}")

    elif action == "loop":
        console.print("[bold]Starting auto-improvement loop...[/]")
        cycle = 0
        while iterations == 0 or cycle < iterations:
            cycle += 1
            console.print(f"\n[bold]Cycle {cycle}[/]")
            result = run_improvement_cycle(
                channels=channel_list,
                since_days=since_days or 7,
                limit_per_channel=limit_per_channel,
                max_items=max_items,
                min_severity=severity or "medium",
                harness=harness,
                persona_id=persona,
                dry_run=dry_run,
            )
            collect_stats = result["collect_stats"]
            console.print(
                f"  Collected: +{collect_stats['feedback_items_created']} new, {collect_stats['feedback_items_updated']} updated"
            )
            console.print(f"  Actionable: {result['actionable_items']}")
            if result["dispatched"]:
                console.print(f"  Execution id: {result['execution_id']}")
                console.print(f"  Thread key: {result['thread_key']}")
            elif dry_run and result["actionable_items"]:
                print(result["prompt"])

            if iterations != 0 and cycle >= iterations:
                break
            console.print(f"[dim]Sleeping for {interval_sec}s...[/]")
            try:
                time.sleep(interval_sec)
            except KeyboardInterrupt:
                console.print("\n[yellow]Loop interrupted[/]")
                break

    else:
        console.print(f"[red]Unknown action: {action}[/]")
        console.print(
            "Valid actions: collect, backfill, digest, show, update-status, improve, loop"
        )
        raise typer.Exit(1)


@app.command("channel-emails")
def channel_emails(
    channel: str = typer.Argument(..., help="Channel name (with or without #)"),
    output: str = typer.Option("text", "-o", "--output", help="Output format: text or json"),
):
    """Get email addresses of all members in a channel.

    Examples:
        slack channel-emails eng-ai
        slack channel-emails #general -o json
    """
    import json
    from .client import get_channel_member_emails

    try:
        emails = get_channel_member_emails(channel)
        if output == "json":
            print(json.dumps({"channel": channel, "emails": emails, "count": len(emails)}))
        else:
            if emails:
                console.print(f"[bold]Members of #{channel} ({len(emails)}):[/]")
                for email in emails:
                    console.print(f"  {email}")
            else:
                console.print(f"[yellow]No members with emails found in #{channel}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("user-info")
def user_info(
    user_id: str = typer.Argument(..., help="Slack user ID (e.g., U123ABC)"),
    output: str = typer.Option("text", "-o", "--output", help="Output format: text or json"),
):
    """Get full user profile including email, phone, title, status, and custom fields.

    Examples:
        slack user-info U123ABC
        slack user-info U123ABC -o json
    """
    import json
    from .client import get_user_profile

    try:
        profile = get_user_profile(user_id)

        if output == "json":
            print(json.dumps(profile, indent=2, ensure_ascii=False))
        else:
            console.print(f"[bold]Name:[/] {profile['real_name'] or profile['name']}")
            if profile["display_name"]:
                console.print(f"[bold]Display Name:[/] {profile['display_name']}")
            if profile["title"]:
                console.print(f"[bold]Title:[/] {profile['title']}")
            if profile["email"]:
                console.print(f"[bold]Email:[/] {profile['email']}")
            else:
                console.print("[yellow]No email found[/]")
            if profile["phone"]:
                console.print(f"[bold]Phone:[/] {profile['phone']}")
            if profile["status_text"]:
                console.print(
                    f"[bold]Status:[/] {profile['status_emoji']} {profile['status_text']}"
                )
            if profile["timezone"]:
                console.print(f"[bold]Timezone:[/] {profile['tz_label']} ({profile['timezone']})")
            if profile["skype"]:
                console.print(f"[bold]Skype:[/] {profile['skype']}")
            if profile["custom_fields"]:
                for label, value in profile["custom_fields"].items():
                    console.print(f"[bold]{label}:[/] {value}")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
