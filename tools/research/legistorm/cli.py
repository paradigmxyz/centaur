"""CLI for LegiStorm Congressional API."""

import json
from datetime import datetime, timedelta

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="legistorm", help="LegiStorm CLI for congressional data")


@app.command("health")
def health():
    """Assert legistorm connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_staff_retired_ids()
        payload = {"ok": True, "tool": "legistorm", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "legistorm", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import LegiStormClient

    return LegiStormClient()


def get_default_dates() -> tuple[str, str]:
    """Get default date range (last 30 days)."""
    today = datetime.now()
    from_date = (today - timedelta(days=30)).strftime("%Y-%m-%d")
    to_date = today.strftime("%Y-%m-%d")
    return from_date, to_date


def truncate(text: str | None, max_len: int = 50) -> str:
    """Truncate text to max length."""
    if not text:
        return ""
    if len(text) <= max_len:
        return text
    return text[: max_len - 3] + "..."


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def extract_items(data: dict | list) -> list[dict]:
    """Normalize LegiStorm responses into a list of row dicts."""
    if isinstance(data, list):
        return [row for row in data if isinstance(row, dict)]
    if isinstance(data, dict):
        for key in ("data", "results", "items"):
            value = data.get(key)
            if isinstance(value, list):
                return [row for row in value if isinstance(row, dict)]
    return []


def member_fields(row: dict) -> tuple[str, str, str, str, str]:
    """Extract display fields from a member row."""
    member = row.get("member", row)
    profile = member.get("profile", {}) if isinstance(member, dict) else {}
    name = (
        f"{profile.get('preferred_first_name') or profile.get('first_name') or member.get('first_name', '')} "
        f"{profile.get('preferred_last_name') or profile.get('last_name') or member.get('last_name', '')}"
    ).strip()

    office = next(
        (office for office in row.get("member_offices", []) if office.get("status") == "In Office"),
        {},
    )
    state = office.get("state_id") or row.get("state") or ""
    party = (
        office.get("party")
        or row.get("party")
        or profile.get("bio_details", {}).get("party_name")
        or ""
    )
    chamber = office.get("office_type_id") or row.get("chamber") or ""
    member_id = member.get("member_id") or row.get("id") or ""
    return str(member_id), name, state, party, chamber


def staff_fields(row: dict) -> tuple[str, str, str, str]:
    """Extract display fields from a staff row."""
    staff = row.get("staff", row)
    name = (
        f"{staff.get('preferred_first_name') or staff.get('first_name', '')} "
        f"{staff.get('preferred_last_name') or staff.get('last_name', '')}"
    ).strip()
    current_titles = [
        position.get("position_title", "")
        for position in row.get("positions", [])
        if position.get("is_current")
    ]
    title = " | ".join(title for title in current_titles if title) or row.get("title", "") or ""
    emails = row.get("staff_emails", [])
    email = emails[0].get("contact_string", "") if emails else row.get("email", "") or ""
    staff_id = staff.get("id") or row.get("id") or ""
    return str(staff_id), name, title, email


@app.command()
def members(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 1000)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    member_id: int = typer.Option(None, "--id", help="Specific member ID"),
    state: str = typer.Option(None, "--state", "-s", help="State postal code (e.g., CA, NY)"),
    status: str = typer.Option("a", "--status", help="a=all, c=current, i=incoming, d=departing"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get congressional members."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_members(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            member_id=member_id,
            state_id=state,
            status=status,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = extract_items(data)
    if not items:
        console.print("[yellow]No members found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for m in items:
            rows.append(list(member_fields(m)))
        print_markdown_table(["ID", "Name", "State", "Party", "Chamber"], rows)
        return

    table = Table(title="Congressional Members")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("State", style="yellow")
    table.add_column("Party", style="green")
    table.add_column("Chamber", style="blue")

    for m in items:
        table.add_row(*member_fields(m))

    console.print(table)


@app.command()
def staff(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 1000)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    staff_id: int = typer.Option(None, "--id", help="Specific staff ID"),
    member_id: int = typer.Option(None, "--member-id", help="Staff for specific member"),
    office_id: int = typer.Option(None, "--office-id", help="Staff for specific office"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get congressional staff."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_staff(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            staff_id=staff_id,
            member_id=member_id,
            office_id=office_id,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = extract_items(data)
    if not items:
        console.print("[yellow]No staff found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for s in items:
            staff_id_value, name, title, email = staff_fields(s)
            rows.append(
                [
                    staff_id_value,
                    name,
                    truncate(title, 30),
                    email,
                ]
            )
        print_markdown_table(["ID", "Name", "Title", "Email"], rows)
        return

    table = Table(title="Congressional Staff")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("Title", style="yellow", max_width=30)
    table.add_column("Email", style="green")

    for s in items:
        staff_id_value, name, title, email = staff_fields(s)
        table.add_row(staff_id_value, name, truncate(title, 30), email)

    console.print(table)


@app.command("staff-portfolios")
def staff_portfolios(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 1000)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    staff_id: int = typer.Option(None, "--id", help="Specific staff ID"),
    member_id: int = typer.Option(None, "--member-id", help="Staff for specific member"),
    office_id: int = typer.Option(None, "--office-id", help="Staff for specific office"),
    issue_endpoint: str = typer.Option(
        None,
        "--issue-endpoint",
        help="Optional explicit issue endpoint override, e.g. /member/issue/list",
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get staff records enriched with explicit issue portfolios when available."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_staff_with_issue_portfolios(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            staff_id=staff_id,
            member_id=member_id,
            office_id=office_id,
            issue_endpoint=issue_endpoint,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    status = data.get("issue_portfolio_status", "unknown")
    endpoint = data.get("issue_endpoint") or "none"
    console.print(f"[bold]Issue portfolio status:[/] {status}")
    console.print(f"[bold]Issue endpoint:[/] {endpoint}")

    items = data.get("staff", [])
    if not items:
        console.print("[yellow]No staff found[/]")
        raise typer.Exit()

    table = Table(title="Congressional Staff Portfolios")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("Title", style="yellow", max_width=30)
    table.add_column("Issues", style="green", max_width=40)

    for row in items:
        staff_id_value, name, title, _ = staff_fields(row)
        issues = row.get("issues", [])
        table.add_row(staff_id_value, name, truncate(title, 30), truncate(", ".join(issues), 40))

    console.print(table)


@app.command("staff-retired")
def staff_retired(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get IDs of staff no longer employed by Congress."""
    client = get_client()

    try:
        data = client.get_staff_retired_ids()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    ids = data.get("data", [])
    console.print(f"[bold]Retired staff IDs:[/] {len(ids)} total")
    if ids:
        console.print(", ".join(str(i) for i in ids[:50]))
        if len(ids) > 50:
            console.print(f"[dim]... and {len(ids) - 50} more[/]")


@app.command()
def offices(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 1000)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    office_id: int = typer.Option(None, "--id", help="Specific office ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get offices (committees, subcommittees, commissions)."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_offices(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            office_id=office_id,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("data", [])
    if not items:
        console.print("[yellow]No offices found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for o in items:
            rows.append(
                [
                    str(o.get("id", "")),
                    truncate(o.get("name", ""), 50),
                    o.get("type", "") or "",
                    o.get("chamber", "") or "",
                ]
            )
        print_markdown_table(["ID", "Name", "Type", "Chamber"], rows)
        return

    table = Table(title="Offices")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan", max_width=50)
    table.add_column("Type", style="yellow")
    table.add_column("Chamber", style="green")

    for o in items:
        table.add_row(
            str(o.get("id", "")),
            truncate(o.get("name", ""), 50),
            o.get("type", "") or "",
            o.get("chamber", "") or "",
        )

    console.print(table)


@app.command("offices-retired")
def offices_retired(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get IDs of inactive offices."""
    client = get_client()

    try:
        data = client.get_offices_retired_ids()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    ids = data.get("data", [])
    console.print(f"[bold]Retired office IDs:[/] {len(ids)} total")
    if ids:
        console.print(", ".join(str(i) for i in ids[:50]))
        if len(ids) > 50:
            console.print(f"[dim]... and {len(ids) - 50} more[/]")


@app.command()
def caucuses(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 1000)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    caucus_id: int = typer.Option(None, "--id", help="Specific caucus ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get congressional caucuses (requires caucus subscription)."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_caucuses(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            caucus_id=caucus_id,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("data", [])
    if not items:
        console.print("[yellow]No caucuses found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for c in items:
            rows.append(
                [
                    str(c.get("id", "")),
                    truncate(c.get("name", ""), 60),
                ]
            )
        print_markdown_table(["ID", "Name"], rows)
        return

    table = Table(title="Caucuses")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan", max_width=60)

    for c in items:
        table.add_row(
            str(c.get("id", "")),
            truncate(c.get("name", ""), 60),
        )

    console.print(table)


@app.command("caucuses-retired")
def caucuses_retired(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get IDs of inactive/deleted caucuses."""
    client = get_client()

    try:
        data = client.get_caucuses_retired_ids()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    ids = data.get("data", [])
    console.print(f"[bold]Retired caucus IDs:[/] {len(ids)} total")
    if ids:
        console.print(", ".join(str(i) for i in ids[:50]))
        if len(ids) > 50:
            console.print(f"[dim]... and {len(ids) - 50} more[/]")


@app.command()
def townhalls(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 100)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    townhall_id: int = typer.Option(None, "--id", help="Specific town hall ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get town hall events."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_townhalls(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            townhall_id=townhall_id,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("data", [])
    if not items:
        console.print("[yellow]No town halls found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for t in items:
            rows.append(
                [
                    str(t.get("id", "")),
                    t.get("date", "") or "",
                    truncate(t.get("location", ""), 40),
                    t.get("member_name", "") or "",
                ]
            )
        print_markdown_table(["ID", "Date", "Location", "Member"], rows)
        return

    table = Table(title="Town Halls")
    table.add_column("ID", style="dim")
    table.add_column("Date", style="yellow")
    table.add_column("Location", style="cyan", max_width=40)
    table.add_column("Member", style="green")

    for t in items:
        table.add_row(
            str(t.get("id", "")),
            t.get("date", "") or "",
            truncate(t.get("location", ""), 40),
            t.get("member_name", "") or "",
        )

    console.print(table)


@app.command()
def trips(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 100)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    trip_id: int = typer.Option(None, "--id", help="Specific trip ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get privately funded travel."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_trips(
            updated_from=updated_from,
            updated_to=updated_to,
            limit=limit,
            page=page,
            trip_id=trip_id,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("data", [])
    if not items:
        console.print("[yellow]No trips found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for t in items:
            rows.append(
                [
                    str(t.get("id", "")),
                    truncate(t.get("destination", ""), 30),
                    truncate(t.get("sponsor", ""), 30),
                    t.get("traveler_name", "") or "",
                    str(t.get("cost", "")),
                ]
            )
        print_markdown_table(["ID", "Destination", "Sponsor", "Traveler", "Cost"], rows)
        return

    table = Table(title="Privately Funded Travel")
    table.add_column("ID", style="dim")
    table.add_column("Destination", style="cyan", max_width=30)
    table.add_column("Sponsor", style="yellow", max_width=30)
    table.add_column("Traveler", style="green")
    table.add_column("Cost", style="magenta")

    for t in items:
        table.add_row(
            str(t.get("id", "")),
            truncate(t.get("destination", ""), 30),
            truncate(t.get("sponsor", ""), 30),
            t.get("traveler_name", "") or "",
            str(t.get("cost", "")),
        )

    console.print(table)


@app.command()
def hearings(
    updated_from: str = typer.Option(None, "--from", "-f", help="Updated from date (YYYY-MM-DD)"),
    updated_to: str = typer.Option(None, "--to", "-t", help="Updated to date (YYYY-MM-DD)"),
    chamber: str = typer.Option("H", "--chamber", "-c", help="H=House, S=Senate"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results (up to 100)"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    hearing_id: int = typer.Option(None, "--id", help="Specific hearing ID"),
    office_id: int = typer.Option(None, "--office-id", help="Filter by committee/office"),
    hearing_from: str = typer.Option(None, "--hearing-from", help="Hearing date from (YYYY-MM-DD)"),
    hearing_to: str = typer.Option(None, "--hearing-to", help="Hearing date to (YYYY-MM-DD)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get congressional hearings."""
    client = get_client()

    default_from, default_to = get_default_dates()
    updated_from = updated_from or default_from
    updated_to = updated_to or default_to

    try:
        data = client.get_hearings(
            updated_from=updated_from,
            updated_to=updated_to,
            chamber=chamber,
            limit=limit,
            page=page,
            hearing_id=hearing_id,
            office_id=office_id,
            hearing_date_from=hearing_from,
            hearing_date_to=hearing_to,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("data", [])
    if not items:
        console.print("[yellow]No hearings found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for h in items:
            rows.append(
                [
                    str(h.get("id", "")),
                    h.get("date", "") or "",
                    truncate(h.get("title", ""), 50),
                    h.get("chamber", "") or "",
                    truncate(h.get("committee", ""), 30),
                ]
            )
        print_markdown_table(["ID", "Date", "Title", "Chamber", "Committee"], rows)
        return

    table = Table(title="Hearings")
    table.add_column("ID", style="dim")
    table.add_column("Date", style="yellow")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Chamber", style="green")
    table.add_column("Committee", style="blue", max_width=30)

    for h in items:
        table.add_row(
            str(h.get("id", "")),
            h.get("date", "") or "",
            truncate(h.get("title", ""), 50),
            h.get("chamber", "") or "",
            truncate(h.get("committee", ""), 30),
        )

    console.print(table)


if __name__ == "__main__":
    app()
