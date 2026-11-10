"""CLI for Crunchbase Enterprise API."""

import json

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(
    name="crunchbase", help="Crunchbase Enterprise API CLI for company and funding data"
)


@app.command("health")
def health():
    """Assert crunchbase connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_deleted_entities()
        payload = {"ok": True, "tool": "crunchbase", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "crunchbase", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()

ORG_DEFAULT_FIELDS = [
    "identifier",
    "short_description",
    "categories",
    "location_identifiers",
    "founded_on",
    "num_employees_enum",
    "funding_total",
    "last_funding_type",
    "investor_type",
    "rank_org",
]

PERSON_DEFAULT_FIELDS = [
    "identifier",
    "first_name",
    "last_name",
    "title",
    "primary_organization",
    "gender",
    "linkedin",
]

FUNDING_DEFAULT_FIELDS = [
    "identifier",
    "announced_on",
    "money_raised",
    "investment_type",
    "funded_organization_identifier",
    "lead_investor_identifiers",
    "investor_identifiers",
    "num_investors",
]


def get_client():
    from .client import CrunchbaseClient

    return CrunchbaseClient()


def format_money(value: dict | None) -> str:
    """Format money object with currency."""
    if not value:
        return "N/A"
    amount = value.get("value_usd") or value.get("value")
    currency = value.get("currency", "USD")
    if amount is None:
        return "N/A"
    if amount >= 1e9:
        return f"${amount / 1e9:.2f}B {currency}"
    elif amount >= 1e6:
        return f"${amount / 1e6:.2f}M {currency}"
    elif amount >= 1e3:
        return f"${amount / 1e3:.2f}K {currency}"
    return f"${amount:.0f} {currency}"


def extract_identifier(obj: dict | None) -> str:
    """Extract identifier value from Crunchbase identifier object."""
    if not obj:
        return "N/A"
    if isinstance(obj, dict):
        return obj.get("value") or obj.get("permalink") or obj.get("uuid") or str(obj)
    return str(obj)


def extract_identifiers(items: list | None) -> str:
    """Extract multiple identifiers."""
    if not items:
        return "N/A"
    names = [extract_identifier(i) for i in items[:5]]
    result = ", ".join(names)
    if len(items) > 5:
        result += f" (+{len(items) - 5} more)"
    return result


def extract_locations(items: list | None) -> str:
    """Extract location names."""
    if not items:
        return "N/A"
    return ", ".join(
        i.get("value", "")
        for i in items[:3]
        if i.get("location_type") in ["city", "region", "country"]
    )


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def org(
    entity_id: str = typer.Argument(
        ..., help="Organization permalink or UUID (e.g., tesla-motors)"
    ),
    fields: str = typer.Option(None, "--fields", "-f", help="Comma-separated field_ids"),
    cards: str = typer.Option(None, "--cards", "-c", help="Comma-separated card_ids"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Lookup an organization by permalink or UUID."""
    client = get_client()

    field_ids = fields.split(",") if fields else ORG_DEFAULT_FIELDS
    card_ids = cards.split(",") if cards else None

    data = client.get_organization(entity_id, field_ids=field_ids, card_ids=card_ids)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    props = data.get("properties", {})
    cards_data = data.get("cards", {})

    if markdown:
        name = extract_identifier(props.get("identifier"))
        print(f"# {name}\n")
        print(f"**Description:** {props.get('short_description', 'N/A')}")
        print(f"**Founded:** {props.get('founded_on', 'N/A')}")
        print(f"**Employees:** {props.get('num_employees_enum', 'N/A')}")
        print(f"**Location:** {extract_locations(props.get('location_identifiers'))}")
        print(f"**Categories:** {extract_identifiers(props.get('categories'))}")
        print(f"**Total Funding:** {format_money(props.get('funding_total'))}")
        print(f"**Last Funding Type:** {props.get('last_funding_type', 'N/A')}")
        print(f"**Rank:** {props.get('rank_org', 'N/A')}")
        if cards_data:
            print(f"\n**Cards loaded:** {', '.join(cards_data.keys())}")
        return

    name = extract_identifier(props.get("identifier"))
    console.print(f"\n[bold cyan]{name}[/]\n")
    console.print(f"[dim]{props.get('short_description', '')}[/]\n")

    console.print(f"Founded: [yellow]{props.get('founded_on', 'N/A')}[/]")
    console.print(f"Employees: [yellow]{props.get('num_employees_enum', 'N/A')}[/]")
    console.print(f"Location: {extract_locations(props.get('location_identifiers'))}")
    console.print(f"Categories: {extract_identifiers(props.get('categories'))}")
    console.print(f"Total Funding: [green]{format_money(props.get('funding_total'))}[/]")
    console.print(f"Last Funding Type: {props.get('last_funding_type', 'N/A')}")
    console.print(f"Rank: #{props.get('rank_org', 'N/A')}")

    if cards_data:
        console.print(f"\n[dim]Cards loaded: {', '.join(cards_data.keys())}[/]")


@app.command()
def person(
    entity_id: str = typer.Argument(..., help="Person permalink or UUID"),
    fields: str = typer.Option(None, "--fields", "-f", help="Comma-separated field_ids"),
    cards: str = typer.Option(None, "--cards", "-c", help="Comma-separated card_ids"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Lookup a person by permalink or UUID."""
    client = get_client()

    field_ids = fields.split(",") if fields else PERSON_DEFAULT_FIELDS
    card_ids = cards.split(",") if cards else None

    data = client.get_person(entity_id, field_ids=field_ids, card_ids=card_ids)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    props = data.get("properties", {})

    if markdown:
        name = f"{props.get('first_name', '')} {props.get('last_name', '')}".strip() or "N/A"
        print(f"# {name}\n")
        print(f"**Title:** {props.get('title', 'N/A')}")
        print(f"**Primary Org:** {extract_identifier(props.get('primary_organization'))}")
        print(f"**Gender:** {props.get('gender', 'N/A')}")
        print(f"**LinkedIn:** {props.get('linkedin', 'N/A')}")
        return

    name = f"{props.get('first_name', '')} {props.get('last_name', '')}".strip()
    console.print(f"\n[bold cyan]{name}[/]\n")
    console.print(f"Title: [yellow]{props.get('title', 'N/A')}[/]")
    console.print(f"Primary Org: {extract_identifier(props.get('primary_organization'))}")
    console.print(f"Gender: {props.get('gender', 'N/A')}")
    console.print(f"LinkedIn: {props.get('linkedin', 'N/A')}")


@app.command()
def funding(
    entity_id: str = typer.Argument(..., help="Funding round UUID"),
    fields: str = typer.Option(None, "--fields", "-f", help="Comma-separated field_ids"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Lookup a funding round by UUID."""
    client = get_client()

    field_ids = fields.split(",") if fields else FUNDING_DEFAULT_FIELDS

    data = client.get_funding_round(entity_id, field_ids=field_ids)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    props = data.get("properties", {})
    console.print(f"\nFunding Round: [bold cyan]{extract_identifier(props.get('identifier'))}[/]\n")
    console.print(f"Announced: [yellow]{props.get('announced_on', 'N/A')}[/]")
    console.print(f"Amount: [green]{format_money(props.get('money_raised'))}[/]")
    console.print(f"Type: {props.get('investment_type', 'N/A')}")
    console.print(f"Company: {extract_identifier(props.get('funded_organization_identifier'))}")
    console.print(f"Lead Investors: {extract_identifiers(props.get('lead_investor_identifiers'))}")
    console.print(f"Total Investors: {props.get('num_investors', 'N/A')}")


@app.command()
def card(
    entity_id: str = typer.Argument(..., help="Organization permalink or UUID"),
    card_id: str = typer.Argument(
        ..., help="Card ID (e.g., founders, raised_funding_rounds, investments)"
    ),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    fields: str = typer.Option(None, "--fields", "-f", help="Card field_ids"),
    after_id: str = typer.Option(None, "--after", help="Pagination cursor (UUID)"),
    order: str = typer.Option(None, "--order", help="Sort order (e.g., announced_on desc)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get a specific card for an organization (for paginated data)."""
    client = get_client()

    card_field_ids = fields.split(",") if fields else None

    data = client.get_organization_card(
        entity_id,
        card_id,
        card_field_ids=card_field_ids,
        limit=limit,
        after_id=after_id,
        order=order,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    entities = data.get("entities", [])
    count = data.get("count", len(entities))

    console.print(f"\n[bold]{card_id}[/] for [cyan]{entity_id}[/] ({count} total)\n")

    if not entities:
        console.print("[yellow]No results[/]")
        return

    table = Table()
    first_props = entities[0].get("properties", {})
    for key in list(first_props.keys())[:6]:
        table.add_column(key, max_width=30)

    for entity in entities:
        props = entity.get("properties", {})
        row = []
        for key in list(first_props.keys())[:6]:
            val = props.get(key)
            if isinstance(val, dict):
                row.append(extract_identifier(val) or format_money(val))
            elif isinstance(val, list):
                row.append(extract_identifiers(val))
            else:
                row.append(str(val) if val is not None else "N/A")
        table.add_row(*row)

    console.print(table)


@app.command()
def search(
    collection: str = typer.Argument(
        "organizations",
        help="Collection to search (organizations, people, funding_rounds, acquisitions, investments)",
    ),
    query_json: str = typer.Option(None, "--query", "-q", help="Query filter as JSON"),
    fields: str = typer.Option(None, "--fields", "-f", help="Comma-separated field_ids"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    order: str = typer.Option(
        None,
        "--order",
        help='Sort order as JSON (e.g., \'[{"field_id":"rank_org","sort":"asc"}]\')',
    ),
    after_id: str = typer.Option(None, "--after", help="Pagination cursor"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for entities in a collection."""
    client = get_client()

    if collection == "organizations":
        field_ids = fields.split(",") if fields else ORG_DEFAULT_FIELDS[:6]
    elif collection == "people":
        field_ids = fields.split(",") if fields else PERSON_DEFAULT_FIELDS
    elif collection == "funding_rounds":
        field_ids = fields.split(",") if fields else FUNDING_DEFAULT_FIELDS
    else:
        field_ids = fields.split(",") if fields else ["identifier"]

    query = json.loads(query_json) if query_json else None
    order_list = json.loads(order) if order else None

    search_methods = {
        "organizations": client.search_organizations,
        "people": client.search_people,
        "funding_rounds": client.search_funding_rounds,
        "acquisitions": client.search_acquisitions,
        "investments": client.search_investments,
    }

    if collection not in search_methods:
        console.print(f"[red]Unknown collection: {collection}[/]")
        console.print(f"Valid: {', '.join(search_methods.keys())}")
        raise typer.Exit(1)

    data = search_methods[collection](
        field_ids=field_ids,
        query=query,
        order=order_list,
        limit=limit,
        after_id=after_id,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    entities = data.get("entities", [])
    count = data.get("count", len(entities))

    if not entities:
        console.print(f"[yellow]No results for {collection}[/]")
        return

    if markdown:
        first_props = entities[0].get("properties", {})
        headers = list(first_props.keys())[:6]
        rows = []
        for entity in entities:
            props = entity.get("properties", {})
            row = []
            for key in headers:
                val = props.get(key)
                if isinstance(val, dict):
                    row.append(extract_identifier(val) or format_money(val))
                elif isinstance(val, list):
                    row.append(extract_identifiers(val))
                else:
                    row.append(str(val) if val is not None else "N/A")
            rows.append(row)
        print(f"**{collection}** ({count} total)\n")
        print_markdown_table(headers, rows)
        return

    console.print(f"\n[bold]{collection}[/] ({count} total)\n")

    table = Table()
    first_props = entities[0].get("properties", {})
    for key in list(first_props.keys())[:6]:
        table.add_column(key, max_width=30)

    for entity in entities:
        props = entity.get("properties", {})
        row = []
        for key in list(first_props.keys())[:6]:
            val = props.get(key)
            if isinstance(val, dict):
                row.append(extract_identifier(val) or format_money(val))
            elif isinstance(val, list):
                row.append(extract_identifiers(val))
            else:
                row.append(str(val) if val is not None else "N/A")
        table.add_row(*row)

    console.print(table)


@app.command()
def autocomplete(
    query: str = typer.Argument(..., help="Search query"),
    collections: str = typer.Option(
        None,
        "--collections",
        "-c",
        help="Comma-separated collection_ids (organizations, people, funding_rounds)",
    ),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Autocomplete search for entities."""
    client = get_client()

    collection_ids = collections.split(",") if collections else None

    data = client.autocomplete(query, collection_ids=collection_ids, limit=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    entities = data.get("entities", [])

    if not entities:
        console.print(f"[yellow]No results for '{query}'[/]")
        return

    if markdown:
        rows = []
        for e in entities:
            ident = e.get("identifier", {})
            rows.append(
                [
                    ident.get("entity_def_id", ""),
                    ident.get("value", ""),
                    ident.get("permalink", ""),
                    e.get("short_description", "")[:60] + "..."
                    if len(e.get("short_description", "")) > 60
                    else e.get("short_description", ""),
                ]
            )
        print_markdown_table(["Type", "Name", "Permalink", "Description"], rows)
        return

    table = Table(title=f"Results for '{query}'")
    table.add_column("Type", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("Permalink", style="yellow")
    table.add_column("Description", max_width=40)

    for e in entities:
        ident = e.get("identifier", {})
        desc = e.get("short_description", "")
        if len(desc) > 40:
            desc = desc[:37] + "..."
        table.add_row(
            ident.get("entity_def_id", ""),
            ident.get("value", ""),
            ident.get("permalink", ""),
            desc,
        )

    console.print(table)


@app.command()
def recent_funding(
    min_amount: float = typer.Option(10_000_000, "--min", help="Minimum funding amount (USD)"),
    days: int = typer.Option(30, "--days", "-d", help="Days back to search"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Find recent funding rounds above a threshold."""
    from datetime import datetime, timedelta

    client = get_client()

    since_date = (datetime.now() - timedelta(days=days)).strftime("%Y-%m-%d")

    query = [
        {
            "type": "predicate",
            "field_id": "announced_on",
            "operator_id": "gte",
            "values": [since_date],
        },
        {
            "type": "predicate",
            "field_id": "money_raised",
            "operator_id": "gte",
            "values": [{"value": min_amount, "currency": "USD"}],
        },
    ]

    order = [{"field_id": "announced_on", "sort": "desc"}]

    data = client.search_funding_rounds(
        field_ids=FUNDING_DEFAULT_FIELDS,
        query=query,
        order=order,
        limit=limit,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    entities = data.get("entities", [])
    count = data.get("count", len(entities))

    if not entities:
        console.print(
            f"[yellow]No funding rounds >= ${min_amount / 1e6:.0f}M in last {days} days[/]"
        )
        return

    if markdown:
        rows = []
        for e in entities:
            props = e.get("properties", {})
            rows.append(
                [
                    props.get("announced_on", "N/A"),
                    extract_identifier(props.get("funded_organization_identifier")),
                    props.get("investment_type", "N/A"),
                    format_money(props.get("money_raised")),
                    extract_identifiers(props.get("lead_investor_identifiers")),
                ]
            )
        print(
            f"**Recent Funding Rounds** (>= ${min_amount / 1e6:.0f}M, last {days} days) - {count} total\n"
        )
        print_markdown_table(["Date", "Company", "Type", "Amount", "Lead Investors"], rows)
        return

    console.print(
        f"\n[bold]Recent Funding Rounds[/] (>= ${min_amount / 1e6:.0f}M, last {days} days) - {count} total\n"
    )

    table = Table()
    table.add_column("Date", style="dim")
    table.add_column("Company", style="cyan", max_width=25)
    table.add_column("Type", style="yellow")
    table.add_column("Amount", style="green", justify="right")
    table.add_column("Lead Investors", max_width=30)

    for e in entities:
        props = e.get("properties", {})
        table.add_row(
            props.get("announced_on", "N/A"),
            extract_identifier(props.get("funded_organization_identifier")),
            props.get("investment_type", "N/A"),
            format_money(props.get("money_raised")),
            extract_identifiers(props.get("lead_investor_identifiers")),
        )

    console.print(table)


@app.command()
def raw(
    method: str = typer.Argument("GET", help="HTTP method (GET or POST)"),
    endpoint: str = typer.Argument(
        ..., help="API endpoint (e.g., /entities/organizations/tesla-motors)"
    ),
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
