"""CLI for OpenFEC federal election API."""

import json

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console
from centaur_sdk.cli_tables import Table

app = typer.Typer(name="openfec", help="OpenFEC CLI for federal election data")


@app.command("health")
def health():
    """Assert openfec connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search_candidates(per_page=1)
        payload = {"ok": True, "tool": "openfec", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "openfec", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import OpenFECClient

    return OpenFECClient()


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
def candidates(
    name: str = typer.Option(None, "--name", help="Search by candidate name"),
    state: str = typer.Option(None, "--state", "-s", help="Two-letter state code"),
    party: str = typer.Option(None, "--party", help="Party code (DEM, REP)"),
    office: str = typer.Option(None, "--office", help="H=House, S=Senate, P=President"),
    cycle: int = typer.Option(None, "--cycle", help="Election cycle year"),
    limit: int = typer.Option(20, "--limit", "-n", help="Results per page"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search/list candidates."""
    client = get_client()

    try:
        data = client.search_candidates(
            name=name,
            state=state,
            party=party,
            office=office,
            cycle=cycle,
            per_page=limit,
            page=page,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No candidates found[/]")
        raise typer.Exit()

    headers = ["Candidate ID", "Name", "State", "Party", "Office", "Election Year"]

    if markdown:
        rows = []
        for c in items:
            rows.append(
                [
                    c.get("candidate_id", "") or "",
                    c.get("name", "") or "",
                    c.get("state", "") or "",
                    c.get("party", "") or "",
                    c.get("office", "") or "",
                    str(c.get("election_year", "") or ""),
                ]
            )
        print_markdown_table(headers, rows)
        return

    table = Table(title="Candidates")
    table.add_column("Candidate ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("State", style="yellow")
    table.add_column("Party", style="green")
    table.add_column("Office", style="blue")
    table.add_column("Election Year", style="magenta")

    for c in items:
        table.add_row(
            c.get("candidate_id", "") or "",
            c.get("name", "") or "",
            c.get("state", "") or "",
            c.get("party", "") or "",
            c.get("office", "") or "",
            str(c.get("election_year", "") or ""),
        )

    console.print(table)


@app.command()
def candidate(
    candidate_id: str = typer.Argument(help="FEC candidate ID (e.g., H0CA12183)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get candidate by candidate ID."""
    client = get_client()

    try:
        data = client.get_candidate(candidate_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No candidate found[/]")
        raise typer.Exit()

    headers = ["Candidate ID", "Name", "State", "Party", "Office", "Election Year"]

    if markdown:
        rows = []
        for c in items:
            rows.append(
                [
                    c.get("candidate_id", "") or "",
                    c.get("name", "") or "",
                    c.get("state", "") or "",
                    c.get("party", "") or "",
                    c.get("office", "") or "",
                    str(c.get("election_year", "") or ""),
                ]
            )
        print_markdown_table(headers, rows)
        return

    table = Table(title=f"Candidate: {candidate_id}")
    table.add_column("Candidate ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("State", style="yellow")
    table.add_column("Party", style="green")
    table.add_column("Office", style="blue")
    table.add_column("Election Year", style="magenta")

    for c in items:
        table.add_row(
            c.get("candidate_id", "") or "",
            c.get("name", "") or "",
            c.get("state", "") or "",
            c.get("party", "") or "",
            c.get("office", "") or "",
            str(c.get("election_year", "") or ""),
        )

    console.print(table)


@app.command()
def committees(
    name: str = typer.Option(None, "--name", help="Search by committee name"),
    state: str = typer.Option(None, "--state", "-s", help="Two-letter state code"),
    committee_type: str = typer.Option(None, "--type", help="Committee type code"),
    cycle: int = typer.Option(None, "--cycle", help="Election cycle year"),
    limit: int = typer.Option(20, "--limit", "-n", help="Results per page"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search committees."""
    client = get_client()

    try:
        data = client.search_committees(
            name=name,
            state=state,
            committee_type=committee_type,
            cycle=cycle,
            per_page=limit,
            page=page,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No committees found[/]")
        raise typer.Exit()

    headers = ["Committee ID", "Name", "State", "Type", "Designation"]

    if markdown:
        rows = []
        for c in items:
            rows.append(
                [
                    c.get("committee_id", "") or "",
                    truncate(c.get("name", ""), 50),
                    c.get("state", "") or "",
                    c.get("committee_type_full", "") or "",
                    c.get("designation_full", "") or "",
                ]
            )
        print_markdown_table(headers, rows)
        return

    table = Table(title="Committees")
    table.add_column("Committee ID", style="dim")
    table.add_column("Name", style="cyan", max_width=50)
    table.add_column("State", style="yellow")
    table.add_column("Type", style="green")
    table.add_column("Designation", style="blue")

    for c in items:
        table.add_row(
            c.get("committee_id", "") or "",
            truncate(c.get("name", ""), 50),
            c.get("state", "") or "",
            c.get("committee_type_full", "") or "",
            c.get("designation_full", "") or "",
        )

    console.print(table)


@app.command()
def contributions(
    committee_id: str = typer.Option(None, "--committee-id", help="Recipient committee ID"),
    contributor_name: str = typer.Option(None, "--contributor-name", help="Contributor name"),
    contributor_state: str = typer.Option(None, "--contributor-state", help="Contributor state"),
    min_amount: float = typer.Option(None, "--min-amount", help="Minimum amount"),
    max_amount: float = typer.Option(None, "--max-amount", help="Maximum amount"),
    min_date: str = typer.Option(None, "--min-date", help="Min date (YYYY-MM-DD)"),
    max_date: str = typer.Option(None, "--max-date", help="Max date (YYYY-MM-DD)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Results per page"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get itemized contributions (Schedule A)."""
    client = get_client()

    try:
        data = client.get_contributions(
            committee_id=committee_id,
            contributor_name=contributor_name,
            contributor_state=contributor_state,
            min_amount=min_amount,
            max_amount=max_amount,
            min_date=min_date,
            max_date=max_date,
            per_page=limit,
            page=page,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No contributions found[/]")
        raise typer.Exit()

    headers = ["Contributor", "State", "Amount", "Date", "Committee"]

    if markdown:
        rows = []
        for c in items:
            committee = c.get("committee", {}) or {}
            rows.append(
                [
                    truncate(c.get("contributor_name", ""), 30),
                    c.get("contributor_state", "") or "",
                    str(c.get("contribution_receipt_amount", "") or ""),
                    c.get("contribution_receipt_date", "") or "",
                    truncate(committee.get("name", ""), 30),
                ]
            )
        print_markdown_table(headers, rows)
        return

    table = Table(title="Contributions (Schedule A)")
    table.add_column("Contributor", style="cyan", max_width=30)
    table.add_column("State", style="yellow")
    table.add_column("Amount", style="green")
    table.add_column("Date", style="blue")
    table.add_column("Committee", style="magenta", max_width=30)

    for c in items:
        committee = c.get("committee", {}) or {}
        table.add_row(
            truncate(c.get("contributor_name", ""), 30),
            c.get("contributor_state", "") or "",
            str(c.get("contribution_receipt_amount", "") or ""),
            c.get("contribution_receipt_date", "") or "",
            truncate(committee.get("name", ""), 30),
        )

    console.print(table)


@app.command()
def filings(
    committee_id: str = typer.Option(..., "--committee-id", help="Committee ID (required)"),
    form_type: str = typer.Option(None, "--form-type", help="Filing form type"),
    limit: int = typer.Option(20, "--limit", "-n", help="Results per page"),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get committee filings."""
    client = get_client()

    try:
        data = client.get_filings(
            committee_id=committee_id,
            form_type=form_type,
            per_page=limit,
            page=page,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No filings found[/]")
        raise typer.Exit()

    headers = ["File Number", "Form Type", "Receipt Date", "Total Receipts", "Total Disbursements"]

    if markdown:
        rows = []
        for f in items:
            rows.append(
                [
                    str(f.get("file_number", "") or ""),
                    f.get("form_type", "") or "",
                    f.get("receipt_date", "") or "",
                    str(f.get("total_receipts", "") or ""),
                    str(f.get("total_disbursements", "") or ""),
                ]
            )
        print_markdown_table(headers, rows)
        return

    table = Table(title="Filings")
    table.add_column("File Number", style="dim")
    table.add_column("Form Type", style="cyan")
    table.add_column("Receipt Date", style="yellow")
    table.add_column("Total Receipts", style="green")
    table.add_column("Total Disbursements", style="magenta")

    for f in items:
        table.add_row(
            str(f.get("file_number", "") or ""),
            f.get("form_type", "") or "",
            f.get("receipt_date", "") or "",
            str(f.get("total_receipts", "") or ""),
            str(f.get("total_disbursements", "") or ""),
        )

    console.print(table)


@app.command()
def totals(
    candidate_id: str = typer.Option(..., "--candidate-id", help="FEC candidate ID (required)"),
    cycle: int = typer.Option(None, "--cycle", help="Election cycle year"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get candidate financial totals."""
    client = get_client()

    try:
        data = client.get_candidate_totals(
            candidate_id=candidate_id,
            cycle=cycle,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No totals found[/]")
        raise typer.Exit()

    headers = ["Cycle", "Receipts", "Disbursements", "Cash On Hand", "Debt"]

    if markdown:
        rows = []
        for t in items:
            rows.append(
                [
                    str(t.get("cycle", "") or ""),
                    str(t.get("receipts", "") or ""),
                    str(t.get("disbursements", "") or ""),
                    str(t.get("cash_on_hand_end_period", "") or ""),
                    str(t.get("debts_owed_by_committee", "") or ""),
                ]
            )
        print_markdown_table(headers, rows)
        return

    table = Table(title=f"Financial Totals: {candidate_id}")
    table.add_column("Cycle", style="dim")
    table.add_column("Receipts", style="green")
    table.add_column("Disbursements", style="yellow")
    table.add_column("Cash On Hand", style="cyan")
    table.add_column("Debt", style="red")

    for t in items:
        table.add_row(
            str(t.get("cycle", "") or ""),
            str(t.get("receipts", "") or ""),
            str(t.get("disbursements", "") or ""),
            str(t.get("cash_on_hand_end_period", "") or ""),
            str(t.get("debts_owed_by_committee", "") or ""),
        )

    console.print(table)


if __name__ == "__main__":
    app()
