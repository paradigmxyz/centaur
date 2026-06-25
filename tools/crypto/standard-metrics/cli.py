"""CLI for Standard Metrics API."""

import json

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console
from centaur_sdk.cli_tables import Table

app = typer.Typer(name="standard-metrics", help="Standard Metrics CLI for portfolio company data")


@app.command("health")
def health():
    """Assert standard-metrics connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_companies(page_size=1)
        payload = {"ok": True, "tool": "standard-metrics", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "standard-metrics", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def _get_client():
    from .client import StandardMetricsClient

    return StandardMetricsClient()


@app.command()
def companies(
    name: str = typer.Option(None, "--name", "-n", help="Filter by company name"),
    limit: int = typer.Option(50, "--limit", "-l", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List portfolio companies."""
    client = _get_client()
    data = client.list_companies(page_size=limit, name=name)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No companies found.[/]")
        raise typer.Exit()

    table = Table(title=f"Companies ({data.get('count', len(results))} total)")
    table.add_column("ID", style="dim", max_width=10)
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("Slug", style="white", max_width=25)
    table.add_column("Status", style="green", max_width=15)

    for company in results:
        table.add_row(
            str(company.get("id", ""))[:10],
            company.get("name", ""),
            company.get("slug", ""),
            company.get("status", ""),
        )

    console.print(table)


@app.command()
def company(
    company_id: str = typer.Argument(..., help="Company ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get company details."""
    client = _get_client()
    data = client.get_company(company_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('name', 'Unknown')}[/]")
    console.print(f"[dim]ID: {data.get('id')} | Slug: {data.get('slug', 'N/A')}[/]\n")

    for key in ["status", "website", "description", "industry", "stage"]:
        if data.get(key):
            console.print(f"[bold]{key.title()}:[/] {data.get(key)}")


@app.command()
def metrics(
    company_id: str = typer.Argument(..., help="Company ID or slug"),
    category: str = typer.Option(None, "--category", "-c", help="Filter by metric category"),
    from_date: str = typer.Option(None, "--from", help="From date (ISO format)"),
    to_date: str = typer.Option(None, "--to", help="To date (ISO format)"),
    cadence: str = typer.Option(None, "--cadence", help="Cadence (month, quarter, year)"),
    limit: int = typer.Option(50, "--limit", "-l", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get company metrics."""
    client = _get_client()

    is_slug = not company_id.isdigit()
    data = client.get_metrics(
        company_id=None if is_slug else company_id,
        company_slug=company_id if is_slug else None,
        category=category,
        from_date=from_date,
        to_date=to_date,
        cadence=cadence,
        page_size=limit,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No metrics found.[/]")
        raise typer.Exit()

    table = Table(title=f"Metrics ({data.get('count', len(results))} total)")
    table.add_column("Category", style="cyan", max_width=25)
    table.add_column("Date", style="white", max_width=12)
    table.add_column("Value", style="yellow", justify="right", max_width=20)
    table.add_column("Cadence", style="dim", max_width=12)

    for metric in results:
        value = metric.get("value")
        if isinstance(value, (int, float)):
            value_str = f"{value:,.2f}" if isinstance(value, float) else f"{value:,}"
        else:
            value_str = str(value) if value else ""

        table.add_row(
            metric.get("category", ""),
            metric.get("date", "")[:10] if metric.get("date") else "",
            value_str,
            metric.get("cadence", ""),
        )

    console.print(table)


@app.command()
def financials(
    company_id: str = typer.Argument(..., help="Company ID or slug"),
    from_date: str = typer.Option(None, "--from", help="From date (ISO format)"),
    to_date: str = typer.Option(None, "--to", help="To date (ISO format)"),
    limit: int = typer.Option(50, "--limit", "-l", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get financial data (revenue, burn, runway, etc)."""
    client = _get_client()

    is_slug = not company_id.isdigit()
    financial_categories = ["revenue", "arr", "net_burn", "runway", "cash_in_bank", "gross_margin"]

    all_results = []
    for category in financial_categories:
        try:
            data = client.get_metrics(
                company_id=None if is_slug else company_id,
                company_slug=company_id if is_slug else None,
                category=category,
                from_date=from_date,
                to_date=to_date,
                page_size=limit,
            )
            all_results.extend(data.get("results", []))
        except Exception:
            pass

    if json_output:
        print(json.dumps(all_results, indent=2))
        return

    if not all_results:
        console.print("[yellow]No financial data found.[/]")
        raise typer.Exit()

    table = Table(title="Financial Metrics")
    table.add_column("Category", style="cyan", max_width=20)
    table.add_column("Date", style="white", max_width=12)
    table.add_column("Value", style="yellow", justify="right", max_width=20)

    for metric in sorted(all_results, key=lambda x: (x.get("category", ""), x.get("date", ""))):
        value = metric.get("value")
        if isinstance(value, (int, float)):
            if abs(value) >= 1e6:
                value_str = f"${value / 1e6:.1f}M"
            elif abs(value) >= 1e3:
                value_str = f"${value / 1e3:.1f}K"
            else:
                value_str = f"${value:,.2f}" if isinstance(value, float) else f"${value:,}"
        else:
            value_str = str(value) if value else ""

        table.add_row(
            metric.get("category", ""),
            metric.get("date", "")[:10] if metric.get("date") else "",
            value_str,
        )

    console.print(table)


@app.command()
def documents(
    company_id: str = typer.Argument(..., help="Company ID"),
    source: str = typer.Option(None, "--source", "-s", help="Filter by source"),
    parse_state: str = typer.Option(None, "--state", help="Filter by parse state"),
    limit: int = typer.Option(50, "--limit", "-l", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List company documents."""
    client = _get_client()
    data = client.get_documents(
        company_id=company_id,
        source=source,
        parse_state=parse_state,
        page_size=limit,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No documents found.[/]")
        raise typer.Exit()

    table = Table(title=f"Documents ({data.get('count', len(results))} total)")
    table.add_column("ID", style="dim", max_width=10)
    table.add_column("Name", style="cyan", max_width=40)
    table.add_column("Source", style="white", max_width=15)
    table.add_column("Status", style="green", max_width=15)
    table.add_column("Uploaded", style="dim", max_width=12)

    for doc in results:
        table.add_row(
            str(doc.get("id", ""))[:10],
            doc.get("name", doc.get("filename", "")),
            doc.get("source", ""),
            doc.get("parse_state", ""),
            doc.get("uploaded_at", "")[:10] if doc.get("uploaded_at") else "",
        )

    console.print(table)


@app.command()
def funds(
    limit: int = typer.Option(50, "--limit", "-l", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List funds."""
    client = _get_client()
    data = client.get_funds(page_size=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    results = data.get("results", [])
    if not results:
        console.print("[yellow]No funds found.[/]")
        raise typer.Exit()

    table = Table(title=f"Funds ({data.get('count', len(results))} total)")
    table.add_column("ID", style="dim", max_width=10)
    table.add_column("Name", style="cyan", max_width=40)
    table.add_column("Slug", style="white", max_width=25)

    for fund in results:
        table.add_row(
            str(fund.get("id", ""))[:10],
            fund.get("name", ""),
            fund.get("slug", ""),
        )

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /companies)"),
    method: str = typer.Option("GET", "--method", "-X", help="HTTP method"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
):
    """Make a raw API call."""
    client = _get_client()

    query_params = None
    if params:
        query_params = {}
        for pair in params.split(","):
            if "=" in pair:
                k, v = pair.split("=", 1)
                query_params[k.strip()] = v.strip()

    try:
        data = client.raw_request(method.upper(), endpoint, params=query_params)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
