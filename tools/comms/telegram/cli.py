"""CLI for Telegram bot operations."""

import asyncio
import json

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="telegram", help="Telegram CLI for AI agents")


@app.command("health")
def health():
    """Assert telegram connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = asyncio.run(client.get_me())
        payload = {"ok": True, "tool": "telegram", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "telegram", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


@app.command()
def send(
    chat_id: str = typer.Argument(..., help="Chat ID or @username"),
    message: str = typer.Argument(..., help="Message text to send"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Parse as Markdown"),
    html: bool = typer.Option(False, "--html", help="Parse as HTML"),
    reply_to: int = typer.Option(None, "--reply-to", "-r", help="Message ID to reply to"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Send a message to a chat or user."""
    from .client import TelegramClient

    parse_mode = None
    if markdown:
        parse_mode = "Markdown"
    elif html:
        parse_mode = "HTML"

    client = TelegramClient()
    result = asyncio.run(
        client.send_message(
            chat_id=chat_id,
            text=message,
            parse_mode=parse_mode,
            reply_to_message_id=reply_to,
        )
    )

    if json_output:
        print(json.dumps(result, indent=2))
    else:
        console.print(f"[green]✓[/] Sent to [cyan]{result['chat_title'] or result['chat_id']}[/]")
        console.print(f"  Message ID: {result['message_id']}")


@app.command()
def updates(
    limit: int = typer.Option(20, "--limit", "-n", help="Max updates to fetch"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full message text"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    watch: bool = typer.Option(
        False, "--watch", "-w", help="Watch for new messages (long polling)"
    ),
):
    """Get recent messages sent to the bot."""
    from .client import TelegramClient

    client = TelegramClient()

    if json_output and not watch:
        results = asyncio.run(client.get_updates(limit=limit))
        print(json.dumps(results, indent=2))
        return

    async def poll():
        offset = None
        while True:
            msgs = await client.get_updates(
                limit=limit if offset is None else 10,
                timeout=5 if watch else 0,
                offset=offset,
            )

            for msg in msgs:
                offset = msg["update_id"] + 1
                user = f"@{msg['from_user']}" if msg["from_user"] else f"id:{msg['from_id']}"
                chat = msg["chat_title"] or f"id:{msg['chat_id']}"

                if full:
                    console.print(f"\n[cyan]{chat}[/] | [green]{user}[/] | {msg['date']}")
                    console.print(msg["text"])
                else:
                    text = (msg["text"] or "")[:80].replace("\n", " ")
                    if len(msg["text"] or "") > 80:
                        text += "..."
                    console.print(f"[cyan]{chat}[/] [green]{user}[/]: {text}")

            if not watch:
                break

    try:
        asyncio.run(poll())
    except KeyboardInterrupt:
        console.print("\n[yellow]Stopped watching.[/]")


@app.command()
def chat(
    chat_id: str = typer.Argument(..., help="Chat ID or @username"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get info about a chat, group, or channel."""
    from .client import TelegramClient

    client = TelegramClient()
    result = asyncio.run(client.get_chat(chat_id=chat_id))

    if json_output:
        print(json.dumps(result, indent=2))
    else:
        table = Table(title=f"Chat: {result.get('title') or result.get('username') or chat_id}")
        table.add_column("Property", style="cyan")
        table.add_column("Value", style="white")

        table.add_row("ID", str(result["id"]))
        table.add_row("Type", result["type"])
        if result.get("title"):
            table.add_row("Title", result["title"])
        if result.get("username"):
            table.add_row("Username", f"@{result['username']}")
        if result.get("description"):
            table.add_row("Description", result["description"][:100])
        if result.get("member_count"):
            table.add_row("Members", str(result["member_count"]))

        console.print(table)


@app.command()
def me(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get info about the bot."""
    from .client import TelegramClient

    client = TelegramClient()
    result = asyncio.run(client.get_me())

    if json_output:
        print(json.dumps(result, indent=2))
    else:
        console.print(f"[bold]Bot:[/] @{result['username']} ({result['first_name']})")
        console.print(f"[dim]ID: {result['id']}[/]")
        console.print(f"Can join groups: {result['can_join_groups']}")
        console.print(f"Can read group messages: {result['can_read_all_group_messages']}")


@app.command()
def forward(
    to_chat: str = typer.Argument(..., help="Destination chat ID or @username"),
    from_chat: str = typer.Argument(..., help="Source chat ID"),
    message_id: int = typer.Argument(..., help="Message ID to forward"),
):
    """Forward a message to another chat."""
    from .client import TelegramClient

    client = TelegramClient()
    result = asyncio.run(
        client.forward_message(
            chat_id=to_chat,
            from_chat_id=from_chat,
            message_id=message_id,
        )
    )

    console.print(f"[green]✓[/] Forwarded as message {result['message_id']}")


@app.command()
def delete(
    chat_id: str = typer.Argument(..., help="Chat ID"),
    message_id: int = typer.Argument(..., help="Message ID to delete"),
):
    """Delete a message."""
    from .client import TelegramClient

    client = TelegramClient()
    asyncio.run(client.delete_message(chat_id=chat_id, message_id=message_id))
    console.print(f"[green]✓[/] Deleted message {message_id}")


@app.command()
def webhook(
    url: str = typer.Argument(None, help="Webhook URL to set (omit to show status)"),
    delete_hook: bool = typer.Option(False, "--delete", "-d", help="Delete current webhook"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Manage webhook for receiving updates."""
    from .client import TelegramClient

    client = TelegramClient()

    if delete_hook:
        asyncio.run(client.delete_webhook())
        console.print("[green]✓[/] Webhook deleted. Bot will use polling.")
        return

    if url:
        asyncio.run(client.set_webhook(url))
        console.print(f"[green]✓[/] Webhook set to: {url}")
        return

    info = asyncio.run(client.get_webhook_info())

    if json_output:
        print(json.dumps(info, indent=2))
    else:
        if info["url"]:
            console.print(f"[bold]Webhook URL:[/] {info['url']}")
            console.print(f"Pending updates: {info['pending_update_count']}")
            if info["last_error_message"]:
                console.print(f"[red]Last error:[/] {info['last_error_message']}")
        else:
            console.print("[dim]No webhook configured. Using polling.[/]")


@app.command()
def login(
    phone: str = typer.Argument(..., help="Phone number with country code (e.g., +1234567890)"),
):
    """Login with your Telegram account (MTProto)."""
    from .user_client import UserClient

    client = UserClient()

    console.print(f"[dim]Sending code to {phone}...[/]")
    result = asyncio.run(client.login(phone))

    if result["status"] != "code_sent":
        console.print(f"[red]Unexpected status: {result}[/]")
        raise typer.Exit(1)

    code = typer.prompt("Enter the code sent to your Telegram")

    verify_result = asyncio.run(
        client.verify_code(
            phone=result["phone"],
            code=code,
            phone_code_hash=result["phone_code_hash"],
        )
    )

    if verify_result.get("status") == "2fa_required":
        password = typer.prompt("Enter your 2FA password", hide_input=True)
        verify_result = asyncio.run(client.verify_2fa(password))

    if verify_result.get("status") == "logged_in":
        console.print(
            f"[green]✓[/] Logged in as @{verify_result.get('username')} ({verify_result.get('first_name')})"
        )
    else:
        console.print(f"[red]Login failed: {verify_result}[/]")
        raise typer.Exit(1)


@app.command()
def history(
    entity: str = typer.Argument(..., help="Chat/channel username or ID"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max messages to fetch"),
    search: str = typer.Option(None, "--search", "-s", help="Search query"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full message text"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get message history from a chat/channel (requires login)."""
    from .user_client import UserClient

    client = UserClient()

    try:
        messages = asyncio.run(
            client.get_messages(
                entity=entity,
                limit=limit,
                search=search,
            )
        )
    finally:
        asyncio.run(client.disconnect())

    if json_output:
        print(json.dumps(messages, indent=2, ensure_ascii=False))
        return

    if not messages:
        console.print("[yellow]No messages found.[/]")
        raise typer.Exit()

    console.print(f"[bold]{entity}[/] - {len(messages)} messages\n")

    for msg in reversed(messages):
        sender = msg["sender_name"] or f"id:{msg['sender_id']}"
        text = msg["text"] or "[no text]"

        if not full and len(text) > 150:
            text = text[:150].replace("\n", " ") + "..."
        elif not full:
            text = text.replace("\n", " ")

        date = msg["date"][:10] if msg["date"] else ""
        console.print(f"[dim]{date}[/] [green]{sender}[/]: {text}")


@app.command()
def dialogs(
    limit: int = typer.Option(50, "--limit", "-n", help="Max dialogs to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List your chats/channels (requires login)."""
    from .user_client import UserClient

    client = UserClient()

    try:
        result = asyncio.run(client.get_dialogs(limit=limit))
    finally:
        asyncio.run(client.disconnect())

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False))
        return

    table = Table(title=f"Dialogs ({len(result)})")
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("Type", style="dim", max_width=10)
    table.add_column("ID", style="white", max_width=20)
    table.add_column("Unread", style="yellow", justify="right", max_width=6)

    for d in result:
        dtype = "channel" if d["is_channel"] else ("group" if d["is_group"] else "user")
        table.add_row(
            d["name"], dtype, str(d["id"]), str(d["unread_count"]) if d["unread_count"] else ""
        )

    console.print(table)


@app.command()
def whoami(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Show logged-in user info (requires login)."""
    from .user_client import UserClient

    client = UserClient()

    try:
        result = asyncio.run(client.get_me())
    finally:
        asyncio.run(client.disconnect())

    if json_output:
        print(json.dumps(result, indent=2))
    else:
        console.print(
            f"[bold]User:[/] @{result['username']} ({result['first_name']} {result.get('last_name') or ''})"
        )
        console.print(f"[dim]ID: {result['id']}[/]")
        console.print(f"[dim]Phone: {result.get('phone', 'N/A')}[/]")


@app.callback()
def main():
    """Telegram CLI for AI agents.

    Bot commands (TELEGRAM_BOT_TOKEN):
        me, send, updates, chat, forward, delete, webhook

    User commands (TELEGRAM_API_ID + TELEGRAM_API_HASH + login):
        login, whoami, dialogs, history
    """
    pass


if __name__ == "__main__":
    app()
