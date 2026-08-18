"""CLI for Arkham Intelligence API."""

from dotenv import load_dotenv

load_dotenv()

import json
from datetime import datetime

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="arkham", help="Arkham Intelligence CLI for blockchain analytics")
console = Console()


def get_client():
    from .client import ArkhamClient

    return ArkhamClient()


def format_usd(value: float | None, decimals: int = 2) -> str:
    """Format USD values with B/M/K suffixes."""
    if value is None:
        return "N/A"
    if abs(value) >= 1e12:
        return f"${value / 1e12:.{decimals}f}T"
    elif abs(value) >= 1e9:
        return f"${value / 1e9:.{decimals}f}B"
    elif abs(value) >= 1e6:
        return f"${value / 1e6:.{decimals}f}M"
    elif abs(value) >= 1e3:
        return f"${value / 1e3:.{decimals}f}K"
    return f"${value:.{decimals}f}"


def format_timestamp(ts: str | None) -> str:
    """Format ISO timestamp to readable date."""
    if not ts:
        return "N/A"
    try:
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
        return dt.strftime("%Y-%m-%d %H:%M")
    except Exception:
        return ts


def truncate(s: str, max_len: int = 20) -> str:
    """Truncate string with ellipsis."""
    if len(s) <= max_len:
        return s
    return s[: max_len - 3] + "..."


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def health():
    """Assert Arkham API connectivity and auth."""
    try:
        details = get_client().health()
    except Exception as exc:
        print(
            json.dumps(
                {"ok": False, "tool": "arkham", "error": str(exc), "details": {}},
                indent=2,
                default=str,
            )
        )
        raise typer.Exit(1) from exc
    payload = {"ok": True, "tool": "arkham", "error": None, "details": details}
    print(json.dumps(payload, indent=2, default=str))


@app.command()
def chains(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List supported blockchains."""
    client = get_client()
    data = client.chains()

    if json_output:
        print(json.dumps(data, indent=2))
        return

    for chain in data:
        console.print(f"• {chain}")


@app.command()
def address(
    addr: str = typer.Argument(..., help="Blockchain address"),
    all_chains: bool = typer.Option(False, "--all", "-a", help="Get data across all chains"),
    enriched: bool = typer.Option(False, "--enriched", "-e", help="Get enriched data with tags"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get intelligence for a blockchain address."""
    client = get_client()

    if enriched:
        data = client.get_address_enriched(addr, include_tags=True, include_clusters=True)
    elif all_chains:
        data = client.get_address_intelligence_all(addr)
    else:
        data = client.get_address_intelligence(addr)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        print(f"# Address: `{addr}`\n")
        if "arkhamEntity" in data and data["arkhamEntity"]:
            entity = data["arkhamEntity"]
            print(f"**Entity:** {entity.get('name', 'Unknown')}")
            if entity.get("type"):
                print(f"**Type:** {entity.get('type')}")
        if "arkhamLabel" in data and data["arkhamLabel"]:
            print(f"**Label:** {data['arkhamLabel'].get('name', 'N/A')}")
        if data.get("chain"):
            print(f"**Chain:** {data.get('chain')}")
        if data.get("contract"):
            print("**Is Contract:** Yes")
        return

    console.print(f"\n[bold cyan]Address:[/] {addr}\n")

    if "arkhamEntity" in data and data["arkhamEntity"]:
        entity = data["arkhamEntity"]
        console.print(f"[bold]Entity:[/] {entity.get('name', 'Unknown')}")
        if entity.get("type"):
            console.print(f"[dim]Type:[/] {entity.get('type')}")
        if entity.get("twitter"):
            console.print(f"[dim]Twitter:[/] {entity.get('twitter')}")

    if "arkhamLabel" in data and data["arkhamLabel"]:
        console.print(f"[bold]Label:[/] {data['arkhamLabel'].get('name', 'N/A')}")

    if data.get("chain"):
        console.print(f"[bold]Chain:[/] {data.get('chain')}")

    if data.get("contract"):
        console.print("[yellow]This is a contract address[/]")

    if "populatedTags" in data and data["populatedTags"]:
        tags = [t.get("label", t.get("id", "")) for t in data["populatedTags"]]
        console.print(f"[bold]Tags:[/] {', '.join(tags)}")


@app.command()
def entity(
    entity_id: str = typer.Argument(..., help="Entity ID (e.g., vitalik-buterin, binance)"),
    summary: bool = typer.Option(False, "--summary", "-s", help="Get summary statistics"),
    predictions: bool = typer.Option(False, "--predictions", "-p", help="Get predicted addresses"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get intelligence for an entity."""
    client = get_client()

    if summary:
        data = client.get_entity_summary(entity_id)
    elif predictions:
        data = client.get_entity_predictions(entity_id)
    else:
        data = client.get_entity(entity_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if summary:
        if markdown:
            print(f"# Entity Summary: {entity_id}\n")
            print(f"**Addresses:** {data.get('numAddresses', 'N/A'):,}")
            print(f"**Total Volume:** {format_usd(data.get('volumeUsd'))}")
            print(f"**Balance:** {format_usd(data.get('balanceUsd'))}")
            print(f"**First Tx:** {format_timestamp(data.get('firstTx'))}")
            print(f"**Last Tx:** {format_timestamp(data.get('lastTx'))}")
        else:
            console.print(f"\n[bold cyan]Entity Summary:[/] {entity_id}\n")
            console.print(f"[bold]Addresses:[/] {data.get('numAddresses', 'N/A'):,}")
            console.print(f"[bold]Total Volume:[/] {format_usd(data.get('volumeUsd'))}")
            console.print(f"[bold]Balance:[/] {format_usd(data.get('balanceUsd'))}")
            console.print(f"[bold]First Tx:[/] {format_timestamp(data.get('firstTx'))}")
            console.print(f"[bold]Last Tx:[/] {format_timestamp(data.get('lastTx'))}")
        return

    if markdown:
        print(f"# Entity: {data.get('name', entity_id)}\n")
        print(f"**ID:** {data.get('id', entity_id)}")
        if data.get("type"):
            print(f"**Type:** {data.get('type')}")
        if data.get("website"):
            print(f"**Website:** {data.get('website')}")
        if data.get("twitter"):
            print(f"**Twitter:** {data.get('twitter')}")
    else:
        console.print(f"\n[bold cyan]{data.get('name', entity_id)}[/]")
        console.print(f"[dim]ID: {data.get('id', entity_id)}[/]\n")
        if data.get("type"):
            console.print(f"[bold]Type:[/] {data.get('type')}")
        if data.get("website"):
            console.print(f"[bold]Website:[/] {data.get('website')}")
        if data.get("twitter"):
            console.print(f"[bold]Twitter:[/] {data.get('twitter')}")

        if "populatedTags" in data and data["populatedTags"]:
            tags = [t.get("label", t.get("id", "")) for t in data["populatedTags"]]
            console.print(f"[bold]Tags:[/] {', '.join(tags)}")


@app.command()
def transfers(
    base: str = typer.Option(None, "--base", "-b", help="Address or entity to filter by"),
    flow: str = typer.Option(None, "--flow", "-f", help="Flow direction: in, out, all"),
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    token: str = typer.Option(None, "--token", "-t", help="Filter by token pricing ID"),
    min_usd: float = typer.Option(None, "--min-usd", help="Minimum USD value"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get token transfers with filters."""
    client = get_client()
    data = client.get_transfers(
        base=base,
        flow=flow,
        chain=chain,
        token_id=token,
        usd_gte=min_usd,
        limit=limit,
    )

    transfers_list = data.get("transfers", []) if isinstance(data, dict) else data

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not transfers_list:
        console.print("[yellow]No transfers found[/]")
        return

    if markdown:
        rows = []
        for tx in transfers_list:
            from_addr = tx.get("fromAddress", {})
            to_addr = tx.get("toAddress", {})
            from_label = from_addr.get("arkhamLabel", {}) or {}
            to_label = to_addr.get("arkhamLabel", {}) or {}
            from_entity = from_addr.get("arkhamEntity", {}) or {}
            to_entity = to_addr.get("arkhamEntity", {}) or {}

            from_name = (
                from_label.get("name")
                or from_entity.get("name")
                or truncate(from_addr.get("address", ""), 16)
            )
            to_name = (
                to_label.get("name")
                or to_entity.get("name")
                or truncate(to_addr.get("address", ""), 16)
            )

            rows.append(
                [
                    format_timestamp(tx.get("blockTimestamp")),
                    from_name,
                    to_name,
                    format_usd(tx.get("unitValue")),
                    tx.get("tokenSymbol", "N/A"),
                    tx.get("chain", "N/A"),
                ]
            )
        print_markdown_table(["Time", "From", "To", "Value", "Token", "Chain"], rows)
        return

    table = Table(title="Transfers")
    table.add_column("Time", style="dim", max_width=18)
    table.add_column("From", style="cyan", max_width=20)
    table.add_column("To", style="green", max_width=20)
    table.add_column("Value", style="yellow", justify="right")
    table.add_column("Token", style="white")
    table.add_column("Chain", style="dim")

    for tx in transfers_list:
        from_addr = tx.get("fromAddress", {})
        to_addr = tx.get("toAddress", {})
        from_label = from_addr.get("arkhamLabel", {}) or {}
        to_label = to_addr.get("arkhamLabel", {}) or {}
        from_entity = from_addr.get("arkhamEntity", {}) or {}
        to_entity = to_addr.get("arkhamEntity", {}) or {}

        from_name = (
            from_label.get("name")
            or from_entity.get("name")
            or truncate(from_addr.get("address", ""), 16)
        )
        to_name = (
            to_label.get("name")
            or to_entity.get("name")
            or truncate(to_addr.get("address", ""), 16)
        )

        table.add_row(
            format_timestamp(tx.get("blockTimestamp")),
            from_name,
            to_name,
            format_usd(tx.get("unitValue")),
            tx.get("tokenSymbol", "N/A"),
            tx.get("chain", "N/A"),
        )

    console.print(table)


@app.command()
def portfolio(
    entity_id: str = typer.Argument(..., help="Entity ID"),
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get entity portfolio holdings."""
    client = get_client()
    data = client.get_entity_portfolio(entity_id, chain=chain)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    holdings = data.get("holdings", []) if isinstance(data, dict) else data

    if not holdings:
        console.print("[yellow]No holdings found[/]")
        return

    if markdown:
        rows = []
        for h in holdings:
            rows.append(
                [
                    h.get("token", {}).get("symbol", "N/A"),
                    f"{h.get('balance', 0):,.4f}",
                    format_usd(h.get("usd")),
                    h.get("chain", "N/A"),
                ]
            )
        print_markdown_table(["Token", "Balance", "USD", "Chain"], rows)
        return

    table = Table(title=f"Portfolio: {entity_id}")
    table.add_column("Token", style="cyan")
    table.add_column("Balance", style="yellow", justify="right")
    table.add_column("USD", style="green", justify="right")
    table.add_column("Chain", style="dim")

    for h in holdings:
        table.add_row(
            h.get("token", {}).get("symbol", "N/A"),
            f"{h.get('balance', 0):,.4f}",
            format_usd(h.get("usd")),
            h.get("chain", "N/A"),
        )

    console.print(table)


@app.command()
def counterparties(
    entity_id: str = typer.Argument(..., help="Entity ID"),
    flow: str = typer.Option(None, "--flow", "-f", help="Flow direction: in, out, all"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get entity counterparties (top interacting addresses)."""
    client = get_client()
    data = client.get_entity_counterparties(entity_id, flow=flow, limit=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    cps = data.get("counterparties", []) if isinstance(data, dict) else data

    if not cps:
        console.print("[yellow]No counterparties found[/]")
        return

    if markdown:
        rows = []
        for cp in cps:
            addr_info = cp.get("address", {})
            label = addr_info.get("arkhamLabel", {}) or {}
            entity_info = addr_info.get("arkhamEntity", {}) or {}
            name = (
                label.get("name")
                or entity_info.get("name")
                or truncate(addr_info.get("address", ""), 16)
            )
            rows.append(
                [
                    name,
                    format_usd(cp.get("usd")),
                    str(cp.get("transactionCount", "N/A")),
                    cp.get("flow", "N/A"),
                ]
            )
        print_markdown_table(["Name", "USD Volume", "Tx Count", "Flow"], rows)
        return

    table = Table(title=f"Counterparties: {entity_id}")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("USD Volume", style="yellow", justify="right")
    table.add_column("Tx Count", style="white", justify="right")
    table.add_column("Flow", style="dim")

    for cp in cps:
        addr_info = cp.get("address", {})
        label = addr_info.get("arkhamLabel", {}) or {}
        entity_info = addr_info.get("arkhamEntity", {}) or {}
        name = (
            label.get("name")
            or entity_info.get("name")
            or truncate(addr_info.get("address", ""), 16)
        )
        table.add_row(
            name,
            format_usd(cp.get("usd")),
            str(cp.get("transactionCount", "N/A")),
            cp.get("flow", "N/A"),
        )

    console.print(table)


@app.command()
def token(
    token_id: str = typer.Argument(..., help="Token pricing ID (e.g., ethereum, bitcoin)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get token intelligence."""
    client = get_client()
    data = client.get_token(token_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(
        f"\n[bold cyan]{data.get('name', token_id)}[/] ({data.get('symbol', '').upper()})"
    )
    console.print(f"[dim]ID: {token_id}[/]")


@app.command("token-holders")
def token_holders(
    token_id: str = typer.Argument(..., help="Token pricing ID (e.g., ethereum, bitcoin)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get top token holders."""
    client = get_client()
    data = client.get_token_holders(token_id, limit=limit)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    holders = data.get("holders", []) if isinstance(data, dict) else data

    if not holders:
        console.print("[yellow]No holders found[/]")
        return

    if markdown:
        rows = []
        for h in holders:
            addr = h.get("address", {})
            label = addr.get("arkhamLabel", {}) or {}
            entity = addr.get("arkhamEntity", {}) or {}
            name = label.get("name") or entity.get("name") or truncate(addr.get("address", ""), 16)
            rows.append(
                [
                    name,
                    f"{h.get('balance', 0):,.4f}",
                    format_usd(h.get("usd")),
                ]
            )
        print_markdown_table(["Holder", "Balance", "USD"], rows)
        return

    table = Table(title=f"Top Holders: {token_id}")
    table.add_column("Holder", style="cyan", max_width=30)
    table.add_column("Balance", style="yellow", justify="right")
    table.add_column("USD", style="green", justify="right")

    for h in holders:
        addr = h.get("address", {})
        label = addr.get("arkhamLabel", {}) or {}
        entity = addr.get("arkhamEntity", {}) or {}
        name = label.get("name") or entity.get("name") or truncate(addr.get("address", ""), 16)
        table.add_row(
            name,
            f"{h.get('balance', 0):,.4f}",
            format_usd(h.get("usd")),
        )

    console.print(table)


@app.command()
def trending(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get trending tokens."""
    client = get_client()
    data = client.get_token_trending(chain=chain)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No trending tokens found[/]")
        return

    if markdown:
        rows = []
        for t in data[:20]:
            change = None
            if t.get("price") and t.get("price24hAgo"):
                change = ((t["price"] - t["price24hAgo"]) / t["price24hAgo"]) * 100
            rows.append(
                [
                    t.get("name", ""),
                    t.get("symbol", "").upper(),
                    f"${t.get('price', 0):,.4f}",
                    f"{change:+.2f}%" if change else "N/A",
                    format_usd(t.get("volume24h")),
                ]
            )
        print_markdown_table(["Name", "Symbol", "Price", "24h Change", "24h Volume"], rows)
        return

    table = Table(title="Trending Tokens")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Symbol", style="yellow")
    table.add_column("Price", style="white", justify="right")
    table.add_column("24h Change", justify="right")
    table.add_column("24h Volume", style="green", justify="right")

    for t in data[:20]:
        change = None
        if t.get("price") and t.get("price24hAgo"):
            change = ((t["price"] - t["price24hAgo"]) / t["price24hAgo"]) * 100
        change_color = "green" if change and change >= 0 else "red"
        table.add_row(
            t.get("name", ""),
            t.get("symbol", "").upper(),
            f"${t.get('price', 0):,.4f}",
            f"[{change_color}]{change:+.2f}%[/]" if change else "N/A",
            format_usd(t.get("volume24h")),
        )

    console.print(table)


@app.command()
def tx(
    tx_hash: str = typer.Argument(..., help="Transaction hash"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get transaction details by hash."""
    client = get_client()
    data = client.get_transaction(tx_hash)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    for chain_name, tx_data in data.items():
        console.print(f"\n[bold cyan]Transaction on {chain_name}[/]\n")
        console.print(f"[bold]Hash:[/] {tx_data.get('hash', tx_hash)}")
        console.print(f"[bold]Block:[/] {tx_data.get('blockNumber', 'N/A')}")
        console.print(f"[bold]Time:[/] {format_timestamp(tx_data.get('blockTimestamp'))}")
        console.print(f"[bold]From:[/] {tx_data.get('fromAddress', 'N/A')}")
        console.print(f"[bold]To:[/] {tx_data.get('toAddress', 'N/A')}")
        console.print(f"[bold]Value:[/] {format_usd(tx_data.get('usdValue'))}")
        console.print(f"[bold]Fee:[/] {tx_data.get('fee', 'N/A')}")


@app.command()
def networks(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get status of all supported networks."""
    client = get_client()
    data = client.get_networks_status()

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for net in data:
            change = net.get("priceChange24hPercent")
            rows.append(
                [
                    net.get("chain", ""),
                    "✓" if net.get("active") else "✗",
                    f"${net.get('price', 0):,.2f}",
                    f"{change:+.2f}%" if change else "N/A",
                    format_usd(net.get("marketCap")),
                ]
            )
        print_markdown_table(["Chain", "Active", "Price", "24h Change", "Market Cap"], rows)
        return

    table = Table(title="Network Status")
    table.add_column("Chain", style="cyan")
    table.add_column("Active", style="green", justify="center")
    table.add_column("Price", style="yellow", justify="right")
    table.add_column("24h Change", justify="right")
    table.add_column("Market Cap", style="white", justify="right")

    for net in data:
        change = net.get("priceChange24hPercent")
        change_color = "green" if change and change >= 0 else "red"
        table.add_row(
            net.get("chain", ""),
            "[green]✓[/]" if net.get("active") else "[red]✗[/]",
            f"${net.get('price', 0):,.2f}",
            f"[{change_color}]{change:+.2f}%[/]" if change else "N/A",
            format_usd(net.get("marketCap")),
        )

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /health, /chains)"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
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

    try:
        data = client._request(endpoint, params=query_params)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
