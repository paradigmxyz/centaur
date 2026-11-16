"""CLI for OpenTable reservation search."""

from dotenv import load_dotenv

load_dotenv()

import json
from datetime import datetime

import typer
from rich.console import Console

from .client import _client

app = typer.Typer(
    name="opentable", help="Search OpenTable for available reservations (search only, cannot book)"
)


@app.command("health")
def health():
    """Assert opentable connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search(term="", limit=1)
        payload = {"ok": True, "tool": "opentable", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "opentable", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def search(
    query: str = typer.Argument("", help="Search term (cuisine, restaurant name, etc.)"),
    covers: int = typer.Option(2, "--covers", "-c", help="Number of people"),
    date: str = typer.Option(None, "--date", "-d", help="Date (YYYY-MM-DD). Default: today"),
    time: str = typer.Option("19:00", "--time", "-t", help="Time (HH:MM). Default: 19:00"),
    zipcode: str = typer.Option(
        None, "--zipcode", "-z", help="Zipcode (e.g., 94105). Auto-sets metro/neighborhoods"
    ),
    metro: str = typer.Option("sf", "--metro", "-m", help="Metro area: sf, nyc, la"),
    region: str = typer.Option(
        None, "--region", "-r", help="Region within metro (e.g., san_francisco, manhattan)"
    ),
    neighborhoods: str = typer.Option(
        None, "--neighborhoods", "-n", help="Comma-separated neighborhood names"
    ),
    limit: int = typer.Option(10, "--limit", "-l", help="Max results to show"),
    sort: str = typer.Option("rating", "--sort", "-s", help="Sort by: rating, distance"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", help="Output as markdown table"),
):
    """Search for available restaurant reservations."""
    ot = _client()

    # If zipcode provided, use it to auto-configure metro and neighborhoods
    neighborhood_ids = None
    region_ids = None
    metro_id = None

    if zipcode:
        zip_info = ot.get_zipcode_info(zipcode)
        if zip_info:
            metro_id = zip_info["metro_id"]
            region_ids = [zip_info["region_id"]] if zip_info.get("region_id") else None
            neighborhood_ids = zip_info["ids"] if zip_info["ids"] else None
            console.print(f"[dim]Using zipcode {zipcode}: {zip_info['name']}[/]")
        else:
            console.print(
                f"[yellow]Zipcode {zipcode} not mapped, falling back to metro '{metro}'[/]"
            )

    if metro_id is None:
        metro_id = ot.get_metro_id(metro)
        if metro_id is None:
            console.print(f"[red]Unknown metro: {metro}. Use: sf, nyc, la[/]")
            raise typer.Exit(1)

    if region_ids is None and region:
        region_id = ot.get_region_id(metro_id, region)
        if region_id:
            region_ids = [region_id]
        else:
            console.print(
                f"[yellow]Unknown region '{region}' for {metro}, searching all regions[/]"
            )

    if neighborhood_ids is None and neighborhoods and region_ids:
        neighborhood_list = [n.strip() for n in neighborhoods.split(",")]
        neighborhood_ids = ot.get_neighborhood_ids(region_ids[0], neighborhood_list)
        if not neighborhood_ids:
            console.print(f"[yellow]No matching neighborhoods found for: {neighborhoods}[/]")

    if date is None:
        date = datetime.now().strftime("%Y-%m-%d")
    date_time = f"{date}T{time}:00"

    console.print(f"[dim]Searching OpenTable for '{query or 'all'}' on {date} at {time}...[/]")

    try:
        results = ot.search(
            term=query,
            covers=covers,
            date_time=date_time,
            metro_id=metro_id,
            region_ids=region_ids,
            neighborhood_ids=neighborhood_ids,
            sort_by=sort,
            limit=limit,
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if not results:
        console.print("[yellow]No restaurants found matching your search[/]")
        return

    if json_output:
        print(json.dumps(results, indent=2))
        return

    if markdown:
        rows = []
        for r in results:
            time_slots = ", ".join(r.get("time_slots", [])[:5])
            rows.append(
                [
                    r.get("name", ""),
                    r.get("cuisine", ""),
                    r.get("neighborhood", ""),
                    time_slots or "No slots",
                ]
            )
        print_markdown_table(["Restaurant", "Cuisine", "Neighborhood", "Available Times"], rows)
        return

    console.print(f"\n[bold cyan]Found {len(results)} restaurants[/]\n")

    for r in results:
        name = r.get("name", "Unknown")
        cuisine = r.get("cuisine", "")
        neighborhood = r.get("neighborhood", "")
        rating = r.get("rating", "")
        price = r.get("price_range", "")
        time_slots = r.get("time_slots", [])

        header = f"[bold]{name}[/]"
        if rating:
            header += f" ({rating})"
        if price:
            header += f" {price}"
        console.print(header)

        meta = []
        if cuisine:
            meta.append(cuisine)
        if neighborhood:
            meta.append(neighborhood)
        if meta:
            console.print(f"  [dim]{' • '.join(meta)}[/]")

        if time_slots:
            slots_str = "  [green]" + "  ".join(time_slots[:6]) + "[/]"
            console.print(slots_str)
        else:
            console.print("  [yellow]No time slots available[/]")
        console.print()


@app.command("metros")
def list_metros_cmd(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List available metro areas."""
    ot = _client()
    metros = ot.list_metros()

    if json_output:
        print(json.dumps(metros, indent=2))
        return

    console.print("\n[bold cyan]Available Metro Areas[/]\n")
    for m in metros:
        console.print(f"  [bold]{m['key']}[/] - {m['name']} (id: {m['id']})")


@app.command("regions")
def list_regions_cmd(
    metro: str = typer.Argument(..., help="Metro area key (sf, nyc, la)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List regions within a metro area."""
    ot = _client()
    metro_id = ot.get_metro_id(metro)
    if metro_id is None:
        console.print(f"[red]Unknown metro: {metro}[/]")
        raise typer.Exit(1)

    regions = ot.list_regions(metro_id)

    if json_output:
        print(json.dumps(regions, indent=2))
        return

    console.print(f"\n[bold cyan]Regions in {metro.upper()}[/]\n")
    for r in regions:
        rid = r["id"] or "all"
        console.print(f"  [bold]{r['key']}[/] - {r['name']} (id: {rid})")


@app.command("neighborhoods")
def list_neighborhoods_cmd(
    metro: str = typer.Argument(..., help="Metro area key"),
    region: str = typer.Argument(..., help="Region key"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List neighborhoods within a region."""
    ot = _client()
    metro_id = ot.get_metro_id(metro)
    if metro_id is None:
        console.print(f"[red]Unknown metro: {metro}[/]")
        raise typer.Exit(1)

    region_id = ot.get_region_id(metro_id, region)
    if region_id is None:
        console.print(f"[red]Unknown region: {region}[/]")
        raise typer.Exit(1)

    neighborhoods = ot.list_neighborhoods(region_id)

    if json_output:
        print(json.dumps(neighborhoods, indent=2))
        return

    if not neighborhoods:
        console.print("[yellow]No neighborhood mappings available for this region[/]")
        return

    console.print(f"\n[bold cyan]Neighborhoods in {region}[/]\n")
    for n in neighborhoods:
        console.print(f"  [bold]{n['name']}[/] (id: {n['id']})")


@app.command("zipcodes")
def list_zipcodes_cmd(
    metro: str = typer.Option(None, "--metro", "-m", help="Filter by metro (sf, nyc, la)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List mapped zipcodes with their neighborhood IDs."""
    ot = _client()
    metro_id = None
    if metro:
        metro_id = ot.get_metro_id(metro)
        if metro_id is None:
            console.print(f"[red]Unknown metro: {metro}[/]")
            raise typer.Exit(1)

    zipcodes = ot.list_zipcodes(metro_id)

    if json_output:
        print(json.dumps(zipcodes, indent=2))
        return

    if not zipcodes:
        console.print("[yellow]No zipcodes mapped[/]")
        return

    console.print(f"\n[bold cyan]Mapped Zipcodes{f' ({metro.upper()})' if metro else ''}[/]\n")
    for z in zipcodes:
        ids_str = ", ".join(str(i) for i in z["ids"]) if z["ids"] else "none"
        console.print(f"  [bold]{z['zipcode']}[/] - {z['name']}")
        console.print(f"    [dim]neighborhoodIds: {ids_str}[/]")


if __name__ == "__main__":
    app()
