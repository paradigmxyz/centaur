"""CLI for Harmonic.AI API."""

import json
from typing import Any

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="harmonic", help="Harmonic.AI API CLI for startup discovery and enrichment")


@app.command("health")
def health():
    """Assert harmonic connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_saved_searches()
        payload = {"ok": True, "tool": "harmonic", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "harmonic", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import HarmonicClient

    return HarmonicClient()


def format_money(value: float | int | None, currency: str = "USD") -> str:
    """Format money value."""
    if value is None:
        return "N/A"
    if value >= 1e9:
        return f"${value / 1e9:.2f}B"
    elif value >= 1e6:
        return f"${value / 1e6:.2f}M"
    elif value >= 1e3:
        return f"${value / 1e3:.2f}K"
    return f"${value:.0f}"


def safe_get(obj: dict | None, *keys: str, default: Any = "N/A") -> Any:
    """Safely navigate nested dicts."""
    if obj is None:
        return default
    current = obj
    for key in keys:
        if isinstance(current, dict):
            current = current.get(key)
        else:
            return default
        if current is None:
            return default
    return current if current is not None else default


def truncate(text: str | None, max_len: int = 80) -> str:
    """Truncate text with ellipsis."""
    if not text:
        return "N/A"
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
def company(
    domain: str = typer.Option(
        None, "--domain", "-d", help="Company website domain (e.g., stripe.com)"
    ),
    url: str = typer.Option(None, "--url", "-u", help="Company website URL"),
    linkedin: str = typer.Option(None, "--linkedin", "-l", help="LinkedIn company URL"),
    twitter: str = typer.Option(None, "--twitter", help="Twitter/X URL"),
    crunchbase: str = typer.Option(None, "--crunchbase", help="Crunchbase URL"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Enrich a company by domain, URL, or social profile."""
    if not any([domain, url, linkedin, twitter, crunchbase]):
        console.print(
            "[red]Error: At least one identifier required (--domain, --url, --linkedin, etc.)[/]"
        )
        raise typer.Exit(1)

    client = get_client()

    try:
        data = client.enrich_company(
            website_domain=domain,
            website_url=url,
            linkedin_url=linkedin,
            twitter_url=twitter,
            crunchbase_url=crunchbase,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = safe_get(data, "name")
    description = safe_get(data, "description")
    website = safe_get(data, "website", "domain")
    headcount = safe_get(data, "headcount")
    funding_total = safe_get(data, "funding", "fundingTotal")
    last_funding_type = safe_get(data, "funding", "lastFundingType")
    founded_date = safe_get(data, "foundedDate") or safe_get(data, "initialized_date")
    customer_type = safe_get(data, "customer_type")
    entity_urn = safe_get(data, "entity_urn")

    tags = data.get("tagsV2") or data.get("tags") or []
    tag_str = ", ".join(
        t.get("displayValue", str(t)) if isinstance(t, dict) else str(t) for t in tags[:5]
    )
    if len(tags) > 5:
        tag_str += f" (+{len(tags) - 5} more)"

    highlights = data.get("highlights") or []
    highlight_str = "; ".join(
        h.get("text", str(h)) if isinstance(h, dict) else str(h) for h in highlights[:3]
    )

    if markdown:
        print(f"# {name}\n")
        print(f"**Website:** {website}")
        print(f"**Description:** {truncate(description, 200)}")
        print(f"**Founded:** {founded_date}")
        print(f"**Headcount:** {headcount}")
        print(f"**Customer Type:** {customer_type}")
        print(
            f"**Total Funding:** {format_money(funding_total) if isinstance(funding_total, (int, float)) else funding_total}"
        )
        print(f"**Last Funding Type:** {last_funding_type}")
        print(f"**Tags:** {tag_str or 'N/A'}")
        print(f"**Highlights:** {highlight_str or 'N/A'}")
        print(f"**URN:** {entity_urn}")
        return

    console.print(f"\n[bold cyan]{name}[/]\n")
    console.print(f"[dim]{truncate(description, 120)}[/]\n")
    console.print(f"Website: [blue]{website}[/]")
    console.print(f"Founded: [yellow]{founded_date}[/]")
    console.print(f"Headcount: [yellow]{headcount}[/]")
    console.print(f"Customer Type: {customer_type}")
    console.print(
        f"Total Funding: [green]{format_money(funding_total) if isinstance(funding_total, (int, float)) else funding_total}[/]"
    )
    console.print(f"Last Funding Type: {last_funding_type}")
    console.print(f"Tags: {tag_str or 'N/A'}")
    if highlight_str:
        console.print(f"Highlights: [dim]{highlight_str}[/]")
    console.print(f"\n[dim]URN: {entity_urn}[/]")


@app.command()
def person(
    linkedin: str = typer.Argument(..., help="LinkedIn profile URL"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Enrich a person by LinkedIn URL."""
    client = get_client()

    try:
        data = client.enrich_person(linkedin)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = (
        safe_get(data, "fullName")
        or f"{safe_get(data, 'firstName', default='')} {safe_get(data, 'lastName', default='')}".strip()
    )
    title = safe_get(data, "title") or safe_get(data, "headline")
    location = safe_get(data, "location")
    email = safe_get(data, "email")
    entity_urn = safe_get(data, "entity_urn")

    experiences = data.get("experience") or data.get("experiences") or []
    current_company = "N/A"
    if experiences:
        first_exp = experiences[0] if isinstance(experiences, list) else experiences
        current_company = safe_get(first_exp, "company", "name") or safe_get(
            first_exp, "companyName"
        )

    if markdown:
        print(f"# {name}\n")
        print(f"**Title:** {title}")
        print(f"**Current Company:** {current_company}")
        print(f"**Location:** {location}")
        print(f"**Email:** {email}")
        print(f"**LinkedIn:** {linkedin}")
        print(f"**URN:** {entity_urn}")
        return

    console.print(f"\n[bold cyan]{name}[/]\n")
    console.print(f"Title: [yellow]{title}[/]")
    console.print(f"Current Company: {current_company}")
    console.print(f"Location: {location}")
    console.print(f"Email: {email}")
    console.print(f"LinkedIn: [blue]{linkedin}[/]")
    console.print(f"\n[dim]URN: {entity_urn}[/]")


@app.command()
def search(
    query: str = typer.Argument(..., help="Natural language search query"),
    size: int = typer.Option(25, "--limit", "-n", help="Max results (1-1000)"),
    threshold: float = typer.Option(
        None, "--threshold", "-t", help="Similarity threshold (0.0-1.0)"
    ),
    cursor: str = typer.Option(None, "--cursor", help="Pagination cursor"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search companies using natural language (Scout Search)."""
    client = get_client()

    try:
        data = client.search_companies_natural_language(
            query=query,
            size=size,
            cursor=cursor,
            similarity_threshold=threshold,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results") or data.get("companies") or []
    page_info = data.get("page_info") or {}
    total = data.get("count") or len(results)

    if not results:
        console.print(f"[yellow]No results for: {query}[/]")
        return

    if markdown:
        rows = []
        for r in results:
            rows.append(
                [
                    safe_get(r, "name"),
                    safe_get(r, "website", "domain") or safe_get(r, "domain"),
                    truncate(safe_get(r, "description"), 60),
                    str(safe_get(r, "headcount")),
                    format_money(safe_get(r, "funding", "fundingTotal"))
                    if isinstance(safe_get(r, "funding", "fundingTotal"), (int, float))
                    else "N/A",
                ]
            )
        print(f"**Results for:** {query} ({total} total)\n")
        print_markdown_table(["Name", "Domain", "Description", "Headcount", "Funding"], rows)
        if page_info.get("next"):
            print(f"\n*Next cursor:* `{page_info['next']}`")
        return

    console.print(f"\n[bold]Results for:[/] {query} ({total} total)\n")

    table = Table()
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Domain", style="blue")
    table.add_column("Description", max_width=40)
    table.add_column("Headcount", justify="right")
    table.add_column("Funding", style="green", justify="right")

    for r in results:
        funding = safe_get(r, "funding", "fundingTotal")
        table.add_row(
            str(safe_get(r, "name")),
            str(safe_get(r, "website", "domain") or safe_get(r, "domain")),
            truncate(safe_get(r, "description"), 40),
            str(safe_get(r, "headcount")),
            format_money(funding) if isinstance(funding, (int, float)) else "N/A",
        )

    console.print(table)
    if page_info.get("next"):
        console.print(f"\n[dim]Next cursor: {page_info['next']}[/]")


@app.command()
def similar(
    company_id: str = typer.Argument(
        ..., help="Company ID or URN (e.g., 123456 or urn:harmonic:company:123456)"
    ),
    size: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get companies similar to a given company."""
    client = get_client()

    try:
        data = client.get_similar_companies(company_id, size=size)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results") or data.get("companies") or []

    if not results:
        console.print(f"[yellow]No similar companies found for: {company_id}[/]")
        return

    if markdown:
        rows = []
        for r in results:
            rows.append(
                [
                    safe_get(r, "name"),
                    safe_get(r, "website", "domain") or safe_get(r, "domain"),
                    truncate(safe_get(r, "description"), 60),
                    str(safe_get(r, "headcount")),
                ]
            )
        print(f"**Similar to:** {company_id}\n")
        print_markdown_table(["Name", "Domain", "Description", "Headcount"], rows)
        return

    console.print(f"\n[bold]Similar to:[/] {company_id}\n")

    table = Table()
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Domain", style="blue")
    table.add_column("Description", max_width=40)
    table.add_column("Headcount", justify="right")

    for r in results:
        table.add_row(
            str(safe_get(r, "name")),
            str(safe_get(r, "website", "domain") or safe_get(r, "domain")),
            truncate(safe_get(r, "description"), 40),
            str(safe_get(r, "headcount")),
        )

    console.print(table)


@app.command()
def typeahead(
    query: str = typer.Argument(..., help="Search query (name, domain, or partial)"),
    search_type: str = typer.Option("COMPANY", "--type", "-t", help="COMPANY, PERSON, or INVESTOR"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Typeahead search for companies, people, or investors."""
    client = get_client()

    try:
        data = client.search_typeahead(query, search_type=search_type.upper())
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results") or []

    if not results:
        console.print(f"[yellow]No results for: {query}[/]")
        return

    if markdown:
        rows = []
        for r in results:
            rows.append(
                [
                    safe_get(r, "name") or safe_get(r, "identifier", "value"),
                    safe_get(r, "entity_urn") or safe_get(r, "id"),
                    truncate(safe_get(r, "description") or safe_get(r, "short_description"), 60),
                ]
            )
        print(f"**Typeahead results for:** {query} ({search_type})\n")
        print_markdown_table(["Name", "ID/URN", "Description"], rows)
        return

    console.print(f"\n[bold]Typeahead results for:[/] {query} ({search_type})\n")

    table = Table()
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("ID/URN", style="dim", max_width=30)
    table.add_column("Description", max_width=40)

    for r in results:
        table.add_row(
            str(safe_get(r, "name") or safe_get(r, "identifier", "value")),
            str(safe_get(r, "entity_urn") or safe_get(r, "id")),
            truncate(safe_get(r, "description") or safe_get(r, "short_description"), 40),
        )

    console.print(table)


@app.command("saved-searches")
def saved_searches(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List all saved searches accessible to your account."""
    client = get_client()

    try:
        data = client.get_saved_searches()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    searches = (
        data if isinstance(data, list) else data.get("savedSearches") or data.get("results") or []
    )

    if not searches:
        console.print("[yellow]No saved searches found[/]")
        return

    if markdown:
        rows = []
        for s in searches:
            rows.append(
                [
                    safe_get(s, "name"),
                    safe_get(s, "id") or safe_get(s, "urn"),
                    safe_get(s, "type") or safe_get(s, "entityType"),
                    str(safe_get(s, "count") or safe_get(s, "resultCount")),
                ]
            )
        print("**Saved Searches**\n")
        print_markdown_table(["Name", "ID/URN", "Type", "Count"], rows)
        return

    console.print("\n[bold]Saved Searches[/]\n")

    table = Table()
    table.add_column("Name", style="cyan", max_width=40)
    table.add_column("ID/URN", style="dim", max_width=30)
    table.add_column("Type")
    table.add_column("Count", justify="right")

    for s in searches:
        table.add_row(
            str(safe_get(s, "name")),
            str(safe_get(s, "id") or safe_get(s, "urn")),
            str(safe_get(s, "type") or safe_get(s, "entityType")),
            str(safe_get(s, "count") or safe_get(s, "resultCount")),
        )

    console.print(table)


@app.command("saved-search-results")
def saved_search_results(
    id_or_urn: str = typer.Argument(..., help="Saved search ID or URN"),
    size: int = typer.Option(50, "--limit", "-n", help="Max results per page"),
    cursor: str = typer.Option(None, "--cursor", help="Pagination cursor"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get results from a saved search."""
    client = get_client()

    try:
        data = client.get_saved_search_results(id_or_urn, cursor=cursor, size=size)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results") or []
    count = data.get("count") or len(results)
    page_info = data.get("page_info") or {}

    if not results:
        console.print(f"[yellow]No results for saved search: {id_or_urn}[/]")
        return

    if markdown:
        rows = []
        for r in results:
            rows.append(
                [
                    safe_get(r, "name"),
                    safe_get(r, "website", "domain") or safe_get(r, "domain"),
                    str(safe_get(r, "headcount")),
                    str(safe_get(r, "id") or safe_get(r, "entity_urn")),
                ]
            )
        print(f"**Saved Search Results:** {id_or_urn} ({count} total)\n")
        print_markdown_table(["Name", "Domain", "Headcount", "ID"], rows)
        if page_info.get("next"):
            print(f"\n*Next cursor:* `{page_info['next']}`")
        return

    console.print(f"\n[bold]Saved Search Results:[/] {id_or_urn} ({count} total)\n")

    table = Table()
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Domain", style="blue")
    table.add_column("Headcount", justify="right")
    table.add_column("ID", style="dim", max_width=25)

    for r in results:
        table.add_row(
            str(safe_get(r, "name")),
            str(safe_get(r, "website", "domain") or safe_get(r, "domain")),
            str(safe_get(r, "headcount")),
            str(safe_get(r, "id") or safe_get(r, "entity_urn")),
        )

    console.print(table)
    if page_info.get("next"):
        console.print(f"\n[dim]Next cursor: {page_info['next']}[/]")


@app.command()
def status(
    ids: str = typer.Option(None, "--ids", help="Comma-separated enrichment IDs"),
    urns: str = typer.Option(None, "--urns", help="Comma-separated enrichment URNs"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Check enrichment status for pending requests."""
    if not ids and not urns:
        console.print("[red]Error: Provide --ids or --urns[/]")
        raise typer.Exit(1)

    client = get_client()

    id_list = ids.split(",") if ids else None
    urn_list = urns.split(",") if urns else None

    try:
        data = client.get_enrichment_status(ids=id_list, urns=urn_list)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    statuses = data if isinstance(data, list) else data.get("statuses") or [data]

    for s in statuses:
        status_val = safe_get(s, "status")
        color = (
            "green"
            if status_val == "COMPLETE"
            else "yellow"
            if status_val in ["QUEUED", "IN_PROGRESS"]
            else "red"
        )
        console.print(f"[{color}]{safe_get(s, 'id') or safe_get(s, 'urn')}[/]: {status_val}")
        if safe_get(s, "enriched_entity_urn") != "N/A":
            console.print(f"  Enriched: {safe_get(s, 'enriched_entity_urn')}")


@app.command()
def raw(
    method: str = typer.Argument("GET", help="HTTP method (GET or POST)"),
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /companies, /search/typeahead)"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
    body: str = typer.Option(None, "--body", "-b", help="Request body as JSON"),
):
    """Make a raw API call."""
    client = get_client()

    query_params = None
    if params:
        query_params = {}
        for pair in params.split(","):
            if "=" in pair:
                k, v = pair.split("=", 1)
                query_params[k.strip()] = v.strip()

    json_body = json.loads(body) if body else None

    try:
        data = client.raw(method, endpoint, params=query_params, json_body=json_body)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
