"""CLI for Federal Register API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="fedreg", help="Federal Register CLI for regulatory data")


@app.command("health")
def health():
    """Assert fedreg connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_agencies()
        payload = {"ok": True, "tool": "fedreg", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "fedreg", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import FederalRegisterClient

    return FederalRegisterClient()


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


def first_agency_name(doc: dict) -> str:
    """Extract the first agency name from a document."""
    agencies = doc.get("agencies", [])
    if agencies and isinstance(agencies[0], dict):
        return agencies[0].get("name", "") or ""
    return ""


def render_search_results(
    data: dict,
    json_output: bool,
    markdown: bool,
    title: str = "Federal Register Documents",
) -> None:
    """Shared renderer for search-style results."""
    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No documents found[/]")
        raise typer.Exit()

    count = data.get("count", len(items))
    console.print(f"[dim]Total results: {count}[/]") if not markdown else None

    headers = ["Doc #", "Title", "Type", "Published", "Agency"]

    rows = []
    for d in items:
        rows.append(
            [
                d.get("document_number", ""),
                truncate(d.get("title", ""), 60),
                d.get("type", "") or "",
                d.get("publication_date", "") or "",
                truncate(first_agency_name(d), 30),
            ]
        )

    if markdown:
        print_markdown_table(headers, rows)
        return

    table = Table(title=title)
    table.add_column("Doc #", style="dim")
    table.add_column("Title", style="cyan", max_width=60)
    table.add_column("Type", style="yellow")
    table.add_column("Published", style="green")
    table.add_column("Agency", style="blue", max_width=30)

    for row in rows:
        table.add_row(*row)

    console.print(table)


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    type: str = typer.Option(
        None, "--type", "-t", help="Document type (RULE/PRORULE/NOTICE/PRESDOCU)"
    ),
    agency: str = typer.Option(
        None, "--agency", "-a", help="Agency slug (e.g. securities-and-exchange-commission)"
    ),
    page: int = typer.Option(1, "--page", "-p", help="Page number"),
    per_page: int = typer.Option(20, "--per-page", "-n", help="Results per page"),
    order: str = typer.Option(
        "relevance", "--order", "-o", help="Sort order (newest/oldest/relevance)"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search Federal Register documents."""
    client = get_client()

    try:
        data = client.search_articles(
            term=query,
            type=type,
            agency=agency,
            page=page,
            per_page=per_page,
            order=order,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    render_search_results(data, json_output, markdown)


@app.command()
def document(
    document_number: str = typer.Argument(..., help="FR document number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get a single document by FR document number."""
    client = get_client()

    try:
        data = client.get_article(document_number)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"[bold cyan]{data.get('title', 'N/A')}[/]")
    console.print()
    console.print(f"[bold]Document #:[/]      {data.get('document_number', '')}")
    console.print(f"[bold]Type:[/]            {data.get('type', '')}")
    console.print(f"[bold]Published:[/]       {data.get('publication_date', '')}")
    console.print(f"[bold]Agency:[/]          {first_agency_name(data)}")
    console.print(f"[bold]Citation:[/]        {data.get('citation', '')}")
    console.print(
        f"[bold]Pages:[/]           {data.get('start_page', '')}-{data.get('end_page', '')}"
    )
    console.print(f"[bold]PDF:[/]             {data.get('pdf_url', '')}")
    console.print(f"[bold]HTML URL:[/]        {data.get('html_url', '')}")

    abstract = data.get("abstract")
    if abstract:
        console.print()
        console.print("[bold]Abstract:[/]")
        console.print(abstract)

    dates = data.get("dates")
    if dates:
        console.print()
        console.print(f"[bold]Dates:[/]           {dates}")

    action = data.get("action")
    if action:
        console.print(f"[bold]Action:[/]          {action}")


@app.command()
def agencies(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List all Federal Register agencies."""
    client = get_client()

    try:
        data = client.get_agencies()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No agencies found[/]")
        raise typer.Exit()

    headers = ["Slug", "Name", "Short Name"]
    rows = []
    for a in data:
        rows.append(
            [
                a.get("slug", ""),
                truncate(a.get("name", ""), 60),
                a.get("short_name", "") or "",
            ]
        )

    if markdown:
        print_markdown_table(headers, rows)
        return

    table = Table(title="Federal Register Agencies")
    table.add_column("Slug", style="dim")
    table.add_column("Name", style="cyan", max_width=60)
    table.add_column("Short Name", style="yellow")

    for row in rows:
        table.add_row(*row)

    console.print(table)


@app.command()
def agency(
    slug: str = typer.Argument(..., help="Agency slug (e.g. securities-and-exchange-commission)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details for a single agency by slug."""
    client = get_client()

    try:
        data = client.get_agency(slug)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"[bold cyan]{data.get('name', 'N/A')}[/]")
    console.print()
    console.print(f"[bold]Slug:[/]            {data.get('slug', '')}")
    console.print(f"[bold]Short Name:[/]      {data.get('short_name', '')}")
    console.print(f"[bold]URL:[/]             {data.get('url', '')}")
    console.print(f"[bold]Description:[/]     {truncate(data.get('description', ''), 100)}")
    console.print(f"[bold]Recent Articles:[/] {data.get('recent_articles_url', '')}")

    child_agencies = data.get("child_agencies", [])
    if child_agencies:
        console.print()
        console.print("[bold]Child Agencies:[/]")
        for child in child_agencies:
            if isinstance(child, dict):
                console.print(f"  • {child.get('name', '')} ({child.get('slug', '')})")


@app.command("public-inspection")
def public_inspection(
    agency: str = typer.Option(None, "--agency", "-a", help="Agency slug filter"),
    type: str = typer.Option(
        None, "--type", "-t", help="Document type (RULE/PRORULE/NOTICE/PRESDOCU)"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get current public inspection documents."""
    client = get_client()

    try:
        data = client.get_public_inspection(agency=agency, type=type)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    items = data.get("results", [])
    if not items:
        console.print("[yellow]No public inspection documents found[/]")
        raise typer.Exit()

    headers = ["Doc #", "Title", "Type", "Agency", "Filed"]
    rows = []
    for d in items:
        rows.append(
            [
                d.get("document_number", ""),
                truncate(d.get("title", ""), 60),
                d.get("type", "") or "",
                truncate(first_agency_name(d), 30),
                d.get("filed_at", "") or "",
            ]
        )

    if markdown:
        print_markdown_table(headers, rows)
        return

    table = Table(title="Public Inspection Documents")
    table.add_column("Doc #", style="dim")
    table.add_column("Title", style="cyan", max_width=60)
    table.add_column("Type", style="yellow")
    table.add_column("Agency", style="blue", max_width=30)
    table.add_column("Filed", style="green")

    for row in rows:
        table.add_row(*row)

    console.print(table)


@app.command("comments-open")
def comments_open(
    agency: str = typer.Option(None, "--agency", "-a", help="Agency slug filter"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for documents with open comment periods."""
    client = get_client()

    try:
        data = client.search_open_comments(agency=agency, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    render_search_results(data, json_output, markdown, title="Documents with Open Comment Periods")


if __name__ == "__main__":
    app()
