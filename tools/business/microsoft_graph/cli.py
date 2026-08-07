"""CLI for Microsoft Graph."""

from __future__ import annotations

import json

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

from .client import MicrosoftGraphClient

app = typer.Typer(name="microsoft-graph", help="Microsoft Graph CLI for AI agents")
console = Console()


def _get_client() -> MicrosoftGraphClient:
    return MicrosoftGraphClient()


@app.command("health")
def health() -> None:
    """Assert Graph connectivity with /me (read-only)."""
    from .client import _client

    client = _client()
    try:
        details = client.me()
        payload = {
            "ok": True,
            "tool": "microsoft_graph",
            "error": None,
            "details": {
                "id": details.get("id"),
                "displayName": details.get("displayName"),
                "mail": details.get("mail") or details.get("userPrincipalName"),
            },
        }
    except Exception as exc:
        payload = {"ok": False, "tool": "microsoft_graph", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        client.close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


@app.command("me")
def me(json_output: bool = typer.Option(False, "--json", help="Output as JSON")) -> None:
    """Show the signed-in Graph user."""
    data = _get_client().me()
    if json_output:
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
        return
    console.print(f"[bold]Name:[/] {data.get('displayName')}")
    console.print(f"[bold]Email:[/] {data.get('mail') or data.get('userPrincipalName')}")
    console.print(f"[bold]Id:[/] {data.get('id')}")


@app.command("list-messages")
def list_messages(
    top: int = typer.Option(10, "--top", "-n", help="Max messages"),
    search: str | None = typer.Option(None, "--search", "-s", help="Optional search string"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
) -> None:
    """List mailbox messages (requires Mail.Read)."""
    data = _get_client().list_messages(top=top, search=search)
    if json_output:
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
        return
    rows = data.get("value") or []
    if not rows:
        console.print("[yellow]No messages found.[/]")
        raise typer.Exit()
    table = Table(title="Mail messages")
    table.add_column("received")
    table.add_column("from")
    table.add_column("subject")
    for row in rows:
        sender = ((row.get("from") or {}).get("emailAddress") or {}).get("address", "")
        table.add_row(
            str(row.get("receivedDateTime", "")),
            str(sender),
            str(row.get("subject", "")),
        )
    console.print(table)


if __name__ == "__main__":
    app()
