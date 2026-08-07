"""CLI for HubSpot CRM."""

from __future__ import annotations

import json

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

from .client import HubspotClient

app = typer.Typer(name="hubspot", help="HubSpot CRM CLI for AI agents")
console = Console()


def _get_client() -> HubspotClient:
    return HubspotClient()


@app.command("health")
def health() -> None:
    """Assert HubSpot connectivity with token metadata (read-only)."""
    from .client import _client

    client = _client()
    try:
        details = client.account_info()
        payload = {
            "ok": True,
            "tool": "hubspot",
            "error": None,
            "details": {
                "portalId": details.get("portalId"),
                "accountType": details.get("accountType"),
                "timeZone": details.get("timeZone"),
                "companyCurrency": details.get("companyCurrency"),
            },
        }
    except Exception as exc:
        payload = {"ok": False, "tool": "hubspot", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        client.close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


@app.command("whoami")
def whoami() -> None:
    """Show HubSpot account details for the current token."""
    info = _get_client().account_info()
    console.print(f"[bold]Portal ID:[/] {info.get('portalId')}")
    console.print(f"[bold]Account type:[/] {info.get('accountType')}")
    console.print(f"[bold]Time zone:[/] {info.get('timeZone')}")
    console.print(f"[bold]Currency:[/] {info.get('companyCurrency')}")


@app.command("search-contacts")
def search_contacts(
    query: str = typer.Argument(..., help="Free-text search query"),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
) -> None:
    """Search HubSpot contacts."""
    data = _get_client().search_contacts(query, limit=limit)
    if json_output:
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
        return
    results = data.get("results") or []
    if not results:
        console.print("[yellow]No contacts found.[/]")
        raise typer.Exit()
    table = Table(title="HubSpot contacts")
    table.add_column("id")
    table.add_column("email")
    table.add_column("name")
    for row in results:
        props = row.get("properties") or {}
        name = f"{props.get('firstname', '')} {props.get('lastname', '')}".strip()
        table.add_row(str(row.get("id", "")), str(props.get("email", "")), name)
    console.print(table)


@app.command("get-contact")
def get_contact(
    contact_id: str = typer.Argument(..., help="HubSpot contact id"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
) -> None:
    """Fetch one HubSpot contact."""
    data = _get_client().get_contact(contact_id)
    if json_output:
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
        return
    console.print(data)


if __name__ == "__main__":
    app()
