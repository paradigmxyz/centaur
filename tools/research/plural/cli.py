"""CLI for Plural (Open States) API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console
from rich.table import Table

from .client import PluralClient

app = typer.Typer(
    name="plural",
    help="Plural (Open States) API — state legislation, legislators, committees, events",
)


@app.command("health")
def health():
    """Assert plural connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_jurisdictions()
        payload = {"ok": True, "tool": "plural", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "plural", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    return PluralClient()


def truncate(text: str | None, max_len: int = 50) -> str:
    if not text:
        return ""
    if len(text) <= max_len:
        return text
    return text[: max_len - 3] + "..."


@app.command()
def jurisdictions(
    classification: str = typer.Option(
        None, "--classification", "-c", help="Filter by type (state, municipality)"
    ),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    per_page: int = typer.Option(52, "--per-page", "-n", help="Results per page"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List available jurisdictions."""
    client = get_client()
    try:
        data = client.list_jurisdictions(
            classification=classification, page=page, per_page=per_page
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No jurisdictions found[/]")
        raise typer.Exit()

    table = Table(title="Jurisdictions")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("Classification", style="yellow")

    for j in results:
        table.add_row(j.get("id", ""), j.get("name", ""), j.get("classification", ""))

    console.print(table)


@app.command()
def people(
    jurisdiction: str = typer.Option(
        None, "--jurisdiction", "-j", help="State name or abbreviation"
    ),
    name: str = typer.Option(None, "--name", help="Filter by name"),
    org_classification: str = typer.Option(
        None, "--role", help="Role (upper, lower, legislature, executive)"
    ),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    per_page: int = typer.Option(10, "--per-page", "-n", help="Results per page"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search legislators, governors, etc."""
    client = get_client()
    try:
        data = client.search_people(
            jurisdiction=jurisdiction,
            name=name,
            org_classification=org_classification,
            page=page,
            per_page=per_page,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No people found[/]")
        raise typer.Exit()

    table = Table(title="People")
    table.add_column("Name", style="cyan")
    table.add_column("Party", style="green")
    table.add_column("Role", style="yellow")
    table.add_column("District", style="dim")
    table.add_column("Jurisdiction", style="magenta")

    for p in results:
        role = p.get("current_role") or {}
        table.add_row(
            p.get("name", ""),
            p.get("party", ""),
            role.get("title", ""),
            role.get("district", ""),
            role.get("org_classification", ""),
        )

    console.print(table)


@app.command()
def bills(
    jurisdiction: str = typer.Option(
        None, "--jurisdiction", "-j", help="State name or abbreviation"
    ),
    session: str = typer.Option(None, "--session", "-s", help="Session identifier"),
    q: str = typer.Option(None, "--query", "-q", help="Full text search"),
    sort: str = typer.Option("updated_desc", "--sort", help="Sort order"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    per_page: int = typer.Option(10, "--per-page", "-n", help="Results per page"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search bills."""
    client = get_client()
    try:
        data = client.search_bills(
            jurisdiction=jurisdiction,
            session=session,
            q=q,
            sort=sort,
            page=page,
            per_page=per_page,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No bills found[/]")
        raise typer.Exit()

    table = Table(title="Bills")
    table.add_column("ID", style="dim")
    table.add_column("Title", style="cyan", max_width=60)
    table.add_column("Session", style="yellow")
    table.add_column("Jurisdiction", style="magenta")
    table.add_column("Updated", style="green")

    for b in results:
        table.add_row(
            b.get("identifier", ""),
            truncate(b.get("title", ""), 60),
            b.get("session", ""),
            b.get("jurisdiction", {}).get("name", "")
            if isinstance(b.get("jurisdiction"), dict)
            else "",
            (b.get("updated_at", "") or "")[:10],
        )

    console.print(table)


@app.command()
def committees(
    jurisdiction: str = typer.Argument(..., help="State name or abbreviation"),
    chamber: str = typer.Option(None, "--chamber", help="Chamber (upper, lower)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    per_page: int = typer.Option(20, "--per-page", "-n", help="Results per page"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List committees for a jurisdiction."""
    client = get_client()
    try:
        data = client.list_committees(
            jurisdiction=jurisdiction, chamber=chamber, page=page, per_page=per_page
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No committees found[/]")
        raise typer.Exit()

    table = Table(title="Committees")
    table.add_column("Name", style="cyan")
    table.add_column("Classification", style="yellow")
    table.add_column("Chamber", style="green")

    for c in results:
        table.add_row(
            c.get("name", ""),
            c.get("classification", ""),
            c.get("parent", {}).get("name", "") if isinstance(c.get("parent"), dict) else "",
        )

    console.print(table)


@app.command()
def events(
    jurisdiction: str = typer.Option(
        None, "--jurisdiction", "-j", help="State name or abbreviation"
    ),
    after: str = typer.Option(None, "--after", help="Events after this datetime (ISO)"),
    before: str = typer.Option(None, "--before", help="Events before this datetime (ISO)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    per_page: int = typer.Option(20, "--per-page", "-n", help="Results per page"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List legislative events."""
    client = get_client()
    try:
        data = client.list_events(
            jurisdiction=jurisdiction, after=after, before=before, page=page, per_page=per_page
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No events found[/]")
        raise typer.Exit()

    table = Table(title="Events")
    table.add_column("Name", style="cyan", max_width=60)
    table.add_column("Start", style="yellow")
    table.add_column("Location", style="green")
    table.add_column("Jurisdiction", style="magenta")

    for e in results:
        loc = e.get("location", {})
        table.add_row(
            truncate(e.get("name", ""), 60),
            (e.get("start_date", "") or "")[:16],
            loc.get("name", "") if isinstance(loc, dict) else "",
            e.get("jurisdiction", {}).get("name", "")
            if isinstance(e.get("jurisdiction"), dict)
            else "",
        )

    console.print(table)


if __name__ == "__main__":
    app()
