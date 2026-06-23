"""CLI for Discord self-token operations."""

import json

from dotenv import load_dotenv

load_dotenv()

import typer  # noqa: E402
from rich.console import Console  # noqa: E402

from centaur_sdk import Table  # noqa: E402

app = typer.Typer(name="discord", help="Discord self-token CLI for AI agents")
console = Console()


def _get_client():
    from .client import DiscordClient

    return DiscordClient()


def _emit(data, json_output: bool):
    if json_output:
        print(json.dumps(data, indent=2, ensure_ascii=False))
        return True
    return False


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
        table.add_row(guild.get("name", ""), guild.get("id", ""), str(guild.get("member_count", "")))
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


@app.command("post")
def post(
    channel: str = typer.Argument(..., help="Channel name or ID"),
    message: str = typer.Argument(..., help="Message text"),
    reply_to: str = typer.Option(None, "--reply-to", "-r", help="Message ID to reply to"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Post a message to a channel."""
    result = _get_client().post_message(
        channel=channel,
        content=message,
        reply_to_message_id=reply_to,
    )
    if _emit(result, json_output):
        return
    console.print(f"[green]Sent[/] message {result.get('id')} to channel {result.get('channel_id')}")


if __name__ == "__main__":
    app()
