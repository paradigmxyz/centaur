"""CLI for Discord self-token operations."""

import json

from dotenv import load_dotenv

load_dotenv()

import typer  # noqa: E402
from rich.console import Console  # noqa: E402
from rich.table import Table  # noqa: E402

app = typer.Typer(name="discord", help="Discord self-token CLI for AI agents")


@app.command("health")
def health():
    """Assert discord connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_me()
        payload = {"ok": True, "tool": "discord", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "discord", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def _get_client():
    from .client import DiscordClient

    return DiscordClient()


def _emit(data, json_output: bool):
    if json_output:
        print(json.dumps(data, indent=2, ensure_ascii=False))
        return True
    return False


def _print_attachments(message):
    """Render a message's attachments so their id/url are visible without --json.

    `discord download <channel> <message_id>` needs the attachment id, and
    `discord download --url` needs the url, so the default output has to surface
    them — otherwise an agent listing messages can't tell a file is there.
    """
    for attachment in message.get("attachments") or []:
        console.print(
            f"  [magenta]📎 {attachment.get('id')}[/] "
            f"{attachment.get('filename', '')} [dim]{attachment.get('url', '')}[/]"
        )


@app.command()
def me(json_output: bool = typer.Option(False, "--json", help="Output as JSON")):
    """Get info about the current user."""
    result = _get_client().get_me()
    if _emit(result, json_output):
        return
    console.print(f"[bold]User:[/] {result.get('username')}#{result.get('discriminator')}")
    console.print(f"[dim]ID: {result.get('id')}[/]")


@app.command("join-server")
def join_server(
    invite: str = typer.Argument(..., help="Discord invite code or URL"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Join a server using an invite."""
    result = _get_client().join_server(invite)
    if _emit(result, json_output):
        return
    guild = result.get("guild", {})
    console.print(f"[green]Joined[/] {guild.get('name') or result.get('code')}")


@app.command("servers")
def servers(
    query: str = typer.Argument("", help="Optional server name filter"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max servers"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List servers/guilds the user is in."""
    results = _get_client().list_servers(query=query, limit=limit)
    if _emit(results, json_output):
        return
    table = Table(title="Discord Servers")
    table.add_column("Name", style="cyan")
    table.add_column("ID", style="dim")
    table.add_column("Members", style="green")
    for guild in results:
        table.add_row(
            guild.get("name", ""), guild.get("id", ""), str(guild.get("member_count", ""))
        )
    console.print(table)


@app.command("channels")
def channels(
    guild: str = typer.Argument(..., help="Server/guild name or ID"),
    query: str = typer.Argument("", help="Optional channel name filter"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List channels in a server/guild."""
    client = _get_client()
    results = client.list_channels(guild, query=query)
    if _emit(results, json_output):
        return
    table = Table(title=f"Discord Channels: {guild}")
    table.add_column("Name", style="cyan")
    table.add_column("ID", style="dim")
    table.add_column("Server", style="green")
    for channel in results:
        table.add_row(channel.get("name", ""), channel.get("id", ""), channel.get("guild_name", ""))
    console.print(table)


@app.command("messages")
def messages(
    channel: str = typer.Argument(..., help="Channel name or ID"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max messages"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Fetch recent messages from a channel."""
    results = _get_client().get_messages(channel=channel, limit=limit)
    if _emit(results, json_output):
        return
    for message in results:
        author = message.get("author", "unknown")
        content = (message.get("content") or "").replace("\n", " ")
        console.print(f"[cyan]{author}[/] [dim]{message.get('timestamp')}[/]: {content}")
        _print_attachments(message)


@app.command("search")
def search(
    query: str = typer.Argument(..., help="Search text"),
    channel: str = typer.Argument(..., help="Channel name or ID"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search messages in one channel visible to the user."""
    results = _get_client().search_messages(
        query=query,
        channel=channel,
        limit=limit,
    )
    if _emit(results, json_output):
        return
    for result in results:
        console.print(
            f"[cyan]#{result.get('channel_name') or result.get('channel_id')}[/] "
            f"[green]{result.get('author')}[/] [dim]{result.get('timestamp')}[/]"
        )
        console.print(result.get("content", ""))
        _print_attachments(result)


@app.command("search-all")
def search_all(
    guild: str = typer.Argument(..., help="Server/guild name or ID"),
    query: str = typer.Argument(..., help="Search text"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search recent messages across visible text channels in a server."""
    results = _get_client().search_all(guild=guild, query=query, limit=limit)
    if _emit(results, json_output):
        return
    for result in results:
        console.print(
            f"[cyan]#{result.get('channel_name') or result.get('channel_id')}[/] "
            f"[green]{result.get('author')}[/] [dim]{result.get('timestamp')}[/]"
        )
        console.print(result.get("content", ""))
        _print_attachments(result)


@app.command("context")
def context(
    channel: str = typer.Argument(..., help="Channel name or ID"),
    message_id: str = typer.Argument(..., help="Message ID"),
    before: int = typer.Option(10, "--before", help="Messages before target"),
    after: int = typer.Option(10, "--after", help="Messages after target"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get messages around a specific message."""
    results = _get_client().get_context(
        channel=channel,
        message_id=message_id,
        before=before,
        after=after,
    )
    if _emit(results, json_output):
        return
    for message in results:
        author = message.get("author", "unknown")
        content = (message.get("content") or "").replace("\n", " ")
        marker = ">" if message.get("id") == message_id else " "
        console.print(f"{marker} [cyan]{author}[/] [dim]{message.get('timestamp')}[/]: {content}")
        _print_attachments(message)


@app.command("post")
def post(
    channel: str = typer.Argument(..., help="Channel name or ID"),
    message: str = typer.Argument(..., help="Message text"),
    reply_to: str = typer.Option(None, "--reply-to", "-r", help="Message ID to reply to"),
    suppress_embeds: bool = typer.Option(
        False, "--suppress-embeds", help="Suppress link/embed unfurling on the message"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Post a message to a channel."""
    result = _get_client().post_message(
        channel=channel,
        content=message,
        reply_to_message_id=reply_to,
        suppress_embeds=suppress_embeds,
    )
    if _emit(result, json_output):
        return
    console.print(
        f"[green]Sent[/] message {result.get('id')} to channel {result.get('channel_id')}"
    )


@app.command("upload")
def upload(
    channel: str = typer.Argument(..., help="Channel name or ID"),
    file_path: str = typer.Argument(..., help="Path to the local file to upload"),
    message: str = typer.Option("", "--message", "-m", help="Optional message text"),
    reply_to: str = typer.Option(None, "--reply-to", "-r", help="Message ID to reply to"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Upload a local file to a channel."""
    result = _get_client().upload_file(
        channel=channel,
        file_path=file_path,
        content=message,
        reply_to_message_id=reply_to,
    )
    if _emit(result, json_output):
        return
    console.print(
        f"[green]Uploaded[/] {file_path} as message {result.get('id')} "
        f"to channel {result.get('channel_id')}"
    )


@app.command("download")
def download(
    channel: str = typer.Argument(
        "", help="Channel name or ID of the message (omit when using --url)"
    ),
    message_id: str = typer.Argument("", help="Message ID whose attachments to download"),
    url: str = typer.Option(None, "--url", help="Download a direct attachment/CDN URL instead"),
    output: str = typer.Option(".", "--output", "-o", help="Output directory"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Download attachments from a message, or a direct attachment URL."""
    client = _get_client()
    if url:
        result = client.download_url(url=url, output_dir=output)
        if _emit(result, json_output):
            return
        console.print(f"[green]Downloaded[/] {result.get('path')}")
        return
    if not channel or not message_id:
        raise typer.BadParameter("Provide CHANNEL and MESSAGE_ID, or --url.")
    results = client.download_message_attachments(
        channel=channel,
        message_id=message_id,
        output_dir=output,
    )
    if _emit(results, json_output):
        return
    if not results:
        console.print("[yellow]No attachments on that message.[/]")
        return
    for saved in results:
        console.print(f"[green]Downloaded[/] {saved.get('path')}")


@app.command("create-thread")
def create_thread(
    channel: str = typer.Argument(..., help="Channel name or ID"),
    name: str = typer.Argument(..., help="Thread name"),
    from_message: str = typer.Option(
        None, "--from-message", "-m", help="Message ID to branch the thread from"
    ),
    content: str = typer.Option(
        None, "--content", "-c", help="First message to post in a standalone thread"
    ),
    private: bool = typer.Option(False, "--private", help="Create a private thread"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Create a thread in a channel."""
    result = _get_client().create_thread(
        channel=channel,
        name=name,
        from_message_id=from_message,
        content=content,
        private=private,
    )
    if _emit(result, json_output):
        return
    console.print(f"[green]Created thread[/] {result.get('name')} ({result.get('id')})")
    console.print(f"[dim]{result.get('url')}[/]")


if __name__ == "__main__":
    app()
