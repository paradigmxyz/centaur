"""CLI for Congress.gov API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console
from rich.table import Table

from .client import CongressClient

app = typer.Typer(name="congress", help="Congress.gov API CLI")


@app.command("health")
def health():
    """Assert congress connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_bills(limit=1)
        payload = {"ok": True, "tool": "congress", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "congress", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    return CongressClient()


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


@app.command()
def bills(
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    bill_type: str = typer.Option(None, "--type", "-t", help="Bill type (hr/s/hjres/sjres)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List bills."""
    client = get_client()
    offset = (page - 1) * limit

    try:
        data = client.list_bills(
            congress=congress,
            bill_type=bill_type,
            limit=limit,
            offset=offset,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("bills", [])
    if not items:
        console.print("[yellow]No bills found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for b in items:
            rows.append(
                [
                    b.get("number", "") or "",
                    b.get("type", "") or "",
                    truncate(b.get("title", ""), 60),
                    b.get("latestAction", {}).get("actionDate", "") or "",
                ]
            )
        print_markdown_table(["Number", "Type", "Title", "Latest Action"], rows)
        return

    table = Table(title=f"Bills — Congress {congress}")
    table.add_column("Number", style="dim")
    table.add_column("Type", style="yellow")
    table.add_column("Title", style="cyan", max_width=60)
    table.add_column("Latest Action", style="green")

    for b in items:
        table.add_row(
            str(b.get("number", "")),
            b.get("type", "") or "",
            truncate(b.get("title", ""), 60),
            b.get("latestAction", {}).get("actionDate", "") or "",
        )

    console.print(table)


@app.command()
def bill(
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    bill_type: str = typer.Option(..., "--type", "-t", help="Bill type (hr/s/hjres/sjres)"),
    number: int = typer.Option(..., "--number", "-n", help="Bill number"),
    detail: str = typer.Option(
        None,
        "--detail",
        "-d",
        help="Sub-resource: actions, amendments, cosponsors, subjects, summaries, text",
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get a specific bill."""
    client = get_client()

    try:
        data = client.get_bill(
            congress=congress,
            bill_type=bill_type,
            number=number,
            detail=detail,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if detail:
        _print_bill_detail(data, detail, markdown)
    else:
        _print_bill_info(data, markdown)


def _print_bill_info(data: dict, markdown: bool) -> None:
    """Print single bill info."""
    b = data.get("bill", {})
    if markdown:
        rows = [
            [
                str(b.get("number", "")),
                b.get("type", "") or "",
                truncate(b.get("title", ""), 80),
                b.get("originChamber", "") or "",
                b.get("latestAction", {}).get("text", "") or "",
            ]
        ]
        print_markdown_table(["Number", "Type", "Title", "Chamber", "Latest Action"], rows)
        return

    table = Table(title=f"{b.get('type', '')} {b.get('number', '')}")
    table.add_column("Field", style="yellow")
    table.add_column("Value", style="cyan")
    table.add_row("Title", b.get("title", ""))
    table.add_row("Type", b.get("type", ""))
    table.add_row("Number", str(b.get("number", "")))
    table.add_row("Origin Chamber", b.get("originChamber", ""))
    table.add_row("Introduced", b.get("introducedDate", ""))
    table.add_row("Latest Action", b.get("latestAction", {}).get("text", ""))
    table.add_row("Latest Action Date", b.get("latestAction", {}).get("actionDate", ""))
    table.add_row(
        "Policy Area", b.get("policyArea", {}).get("name", "") if b.get("policyArea") else ""
    )
    console.print(table)


def _print_bill_detail(data: dict, detail: str, markdown: bool) -> None:
    """Print bill sub-resource."""
    if detail == "actions":
        items = data.get("actions", [])
        if not items:
            console.print("[yellow]No actions found[/]")
            return
        if markdown:
            rows = [[a.get("actionDate", ""), truncate(a.get("text", ""), 80)] for a in items]
            print_markdown_table(["Date", "Action"], rows)
            return
        table = Table(title="Actions")
        table.add_column("Date", style="yellow")
        table.add_column("Action", style="cyan", max_width=80)
        for a in items:
            table.add_row(a.get("actionDate", ""), truncate(a.get("text", ""), 80))
        console.print(table)

    elif detail == "amendments":
        items = data.get("amendments", [])
        if not items:
            console.print("[yellow]No amendments found[/]")
            return
        if markdown:
            rows = [
                [a.get("number", ""), a.get("type", ""), truncate(a.get("description", ""), 60)]
                for a in items
            ]
            print_markdown_table(["Number", "Type", "Description"], rows)
            return
        table = Table(title="Amendments")
        table.add_column("Number", style="dim")
        table.add_column("Type", style="yellow")
        table.add_column("Description", style="cyan", max_width=60)
        for a in items:
            table.add_row(
                str(a.get("number", "")),
                a.get("type", "") or "",
                truncate(a.get("description", ""), 60),
            )
        console.print(table)

    elif detail == "cosponsors":
        items = data.get("cosponsors", [])
        if not items:
            console.print("[yellow]No cosponsors found[/]")
            return
        if markdown:
            rows = [
                [
                    c.get("bioguideId", ""),
                    f"{c.get('firstName', '')} {c.get('lastName', '')}".strip(),
                    c.get("party", "") or "",
                    c.get("state", "") or "",
                ]
                for c in items
            ]
            print_markdown_table(["Bioguide", "Name", "Party", "State"], rows)
            return
        table = Table(title="Cosponsors")
        table.add_column("Bioguide", style="dim")
        table.add_column("Name", style="cyan")
        table.add_column("Party", style="green")
        table.add_column("State", style="yellow")
        for c in items:
            name = f"{c.get('firstName', '')} {c.get('lastName', '')}".strip()
            table.add_row(
                c.get("bioguideId", ""),
                name,
                c.get("party", "") or "",
                c.get("state", "") or "",
            )
        console.print(table)

    elif detail == "subjects":
        items = data.get("subjects", {}).get("legislativeSubjects", [])
        if not items:
            console.print("[yellow]No subjects found[/]")
            return
        if markdown:
            rows = [[s.get("name", "")] for s in items]
            print_markdown_table(["Subject"], rows)
            return
        table = Table(title="Subjects")
        table.add_column("Subject", style="cyan")
        for s in items:
            table.add_row(s.get("name", ""))
        console.print(table)

    elif detail == "summaries":
        items = data.get("summaries", [])
        if not items:
            console.print("[yellow]No summaries found[/]")
            return
        if markdown:
            rows = [
                [s.get("actionDate", ""), s.get("versionCode", ""), truncate(s.get("text", ""), 80)]
                for s in items
            ]
            print_markdown_table(["Date", "Version", "Text"], rows)
            return
        table = Table(title="Summaries")
        table.add_column("Date", style="yellow")
        table.add_column("Version", style="dim")
        table.add_column("Text", style="cyan", max_width=80)
        for s in items:
            table.add_row(
                s.get("actionDate", ""),
                s.get("versionCode", ""),
                truncate(s.get("text", ""), 80),
            )
        console.print(table)

    elif detail == "text":
        items = data.get("textVersions", [])
        if not items:
            console.print("[yellow]No text versions found[/]")
            return
        if markdown:
            rows = [[t.get("date", "") or "", t.get("type", "")] for t in items]
            print_markdown_table(["Date", "Type"], rows)
            return
        table = Table(title="Text Versions")
        table.add_column("Date", style="yellow")
        table.add_column("Type", style="cyan")
        for t in items:
            table.add_row(t.get("date", "") or "", t.get("type", "") or "")
        console.print(table)

    else:
        print(json.dumps(data, indent=2))


@app.command()
def members(
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    state: str = typer.Option(None, "--state", "-s", help="State postal code (e.g., CA, NY)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List members of Congress."""
    client = get_client()

    try:
        data = client.list_members(
            congress=congress,
            state=state,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("members", [])
    if not items:
        console.print("[yellow]No members found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for m in items:
            rows.append(
                [
                    m.get("bioguideId", "") or "",
                    m.get("name", "") or "",
                    m.get("partyName", "") or "",
                    m.get("state", "") or "",
                ]
            )
        print_markdown_table(["Bioguide", "Name", "Party", "State"], rows)
        return

    table = Table(title=f"Members — Congress {congress}")
    table.add_column("Bioguide", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("Party", style="green")
    table.add_column("State", style="yellow")

    for m in items:
        table.add_row(
            m.get("bioguideId", "") or "",
            m.get("name", "") or "",
            m.get("partyName", "") or "",
            m.get("state", "") or "",
        )

    console.print(table)


@app.command()
def member(
    bioguide_id: str = typer.Argument(..., help="Bioguide ID (e.g. L000174)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get a specific member by bioguide ID."""
    client = get_client()

    try:
        data = client.get_member(bioguide_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    m = data.get("member", {})

    if markdown:
        rows = [
            [
                m.get("bioguideId", ""),
                f"{m.get('firstName', '')} {m.get('lastName', '')}".strip(),
                m.get("partyHistory", [{}])[0].get("partyName", "")
                if m.get("partyHistory")
                else "",
                m.get("state", "") or "",
                m.get("district", "") or "",
            ]
        ]
        print_markdown_table(["Bioguide", "Name", "Party", "State", "District"], rows)
        return

    table = Table(title=f"Member: {m.get('firstName', '')} {m.get('lastName', '')}")
    table.add_column("Field", style="yellow")
    table.add_column("Value", style="cyan")
    table.add_row("Bioguide ID", m.get("bioguideId", ""))
    table.add_row("Name", f"{m.get('firstName', '')} {m.get('lastName', '')}".strip())
    table.add_row("Birth Year", m.get("birthYear", "") or "")
    table.add_row("State", m.get("state", "") or "")
    table.add_row("District", str(m.get("district", "")) if m.get("district") else "")
    party = m.get("partyHistory", [{}])[0].get("partyName", "") if m.get("partyHistory") else ""
    table.add_row("Party", party)
    console.print(table)


@app.command()
def committees(
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    chamber: str = typer.Option(None, "--chamber", help="Chamber (house/senate/joint)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List committees."""
    client = get_client()

    try:
        data = client.list_committees(
            congress=congress,
            chamber=chamber,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("committees", [])
    if not items:
        console.print("[yellow]No committees found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for c in items:
            rows.append(
                [
                    c.get("systemCode", "") or "",
                    truncate(c.get("name", ""), 60),
                    c.get("chamber", "") or "",
                ]
            )
        print_markdown_table(["Code", "Name", "Chamber"], rows)
        return

    table = Table(title=f"Committees — Congress {congress}")
    table.add_column("Code", style="dim")
    table.add_column("Name", style="cyan", max_width=60)
    table.add_column("Chamber", style="yellow")

    for c in items:
        table.add_row(
            c.get("systemCode", "") or "",
            truncate(c.get("name", ""), 60),
            c.get("chamber", "") or "",
        )

    console.print(table)


@app.command()
def hearings(
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    chamber: str = typer.Option(None, "--chamber", help="Chamber (house/senate)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List hearings."""
    client = get_client()

    try:
        data = client.list_hearings(
            congress=congress,
            chamber=chamber,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("hearings", [])
    if not items:
        console.print("[yellow]No hearings found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for h in items:
            rows.append(
                [
                    str(h.get("number", "")),
                    h.get("chamber", "") or "",
                    truncate(h.get("title", ""), 60),
                    h.get("date", "") or "",
                ]
            )
        print_markdown_table(["Number", "Chamber", "Title", "Date"], rows)
        return

    table = Table(title=f"Hearings — Congress {congress}")
    table.add_column("Number", style="dim")
    table.add_column("Chamber", style="yellow")
    table.add_column("Title", style="cyan", max_width=60)
    table.add_column("Date", style="green")

    for h in items:
        table.add_row(
            str(h.get("number", "")),
            h.get("chamber", "") or "",
            truncate(h.get("title", ""), 60),
            h.get("date", "") or "",
        )

    console.print(table)


@app.command()
def votes(
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    chamber: str = typer.Option(None, "--chamber", help="Chamber (house/senate)"),
    session: int = typer.Option(None, "--session", "-s", help="Session number (1 or 2)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List roll call votes."""
    client = get_client()

    try:
        data = client.list_votes(
            congress=congress,
            chamber=chamber,
            session=session,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("rollcalls", [])
    if not items:
        console.print("[yellow]No votes found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for v in items:
            rows.append(
                [
                    str(v.get("rollcallNumber", "")),
                    v.get("chamber", "") or "",
                    v.get("date", "") or "",
                    truncate(v.get("question", ""), 50),
                    v.get("result", "") or "",
                ]
            )
        print_markdown_table(["Roll Call", "Chamber", "Date", "Question", "Result"], rows)
        return

    table = Table(title=f"Votes — Congress {congress}")
    table.add_column("Roll Call", style="dim")
    table.add_column("Chamber", style="yellow")
    table.add_column("Date", style="green")
    table.add_column("Question", style="cyan", max_width=50)
    table.add_column("Result", style="magenta")

    for v in items:
        table.add_row(
            str(v.get("rollcallNumber", "")),
            v.get("chamber", "") or "",
            v.get("date", "") or "",
            truncate(v.get("question", ""), 50),
            v.get("result", "") or "",
        )

    console.print(table)


@app.command()
def search(
    keyword: str = typer.Argument(..., help="Keyword to search for in bill titles"),
    congress: int = typer.Option(119, "--congress", "-c", help="Congress number"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search bills by keyword (browse bills sorted by latest action)."""
    client = get_client()

    try:
        data = client.list_bills(congress=congress, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    all_bills = data.get("bills", [])
    kw = keyword.lower()
    matched = [b for b in all_bills if kw in (b.get("title", "") or "").lower()]

    if json_output:
        print(json.dumps(matched, indent=2))
        return

    if not matched:
        console.print(f"[yellow]No bills matching '{keyword}' found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for b in matched:
            rows.append(
                [
                    b.get("number", "") or "",
                    b.get("type", "") or "",
                    truncate(b.get("title", ""), 60),
                    b.get("latestAction", {}).get("actionDate", "") or "",
                ]
            )
        print_markdown_table(["Number", "Type", "Title", "Latest Action"], rows)
        return

    table = Table(title=f"Bills matching '{keyword}'")
    table.add_column("Number", style="dim")
    table.add_column("Type", style="yellow")
    table.add_column("Title", style="cyan", max_width=60)
    table.add_column("Latest Action", style="green")

    for b in matched:
        table.add_row(
            str(b.get("number", "")),
            b.get("type", "") or "",
            truncate(b.get("title", ""), 60),
            b.get("latestAction", {}).get("actionDate", "") or "",
        )

    console.print(table)


if __name__ == "__main__":
    app()
