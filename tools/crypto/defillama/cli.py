"""CLI for DefiLlama API."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

from .client import DefiLlamaClient, _looks_like_perps

load_dotenv()

app = typer.Typer(
    name="defillama",
    help=(
        "DefiLlama CLI for stablecoin and DeFi analytics. Prefer typed commands "
        "over raw endpoints; use derivatives-volume/derivatives-summary/open-interest "
        "for "
        "perps venues such as Hyperliquid, Lighter, GMX, and dYdX."
    ),
)


@app.command("health")
def health():
    """Assert defillama connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_protocols()
        payload = {"ok": True, "tool": "defillama", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "defillama", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    return DefiLlamaClient()


def format_number(value: float, decimals: int = 2) -> str:
    """Format large numbers with B/M/K suffixes."""
    if value >= 1e9:
        return f"${value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"${value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"${value / 1e3:.{decimals}f}K"
    return f"${value:.{decimals}f}"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def stablecoins(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List stablecoins with market caps."""
    client = get_client()
    data = client.list_stablecoins()

    if json_output:
        print(json.dumps(data[:limit], indent=2))
        return

    sorted_data = sorted(
        data, key=lambda x: x.get("circulating", {}).get("peggedUSD", 0), reverse=True
    )[:limit]

    if markdown:
        rows = []
        for coin in sorted_data:
            mcap = coin.get("circulating", {}).get("peggedUSD", 0)
            chains = ", ".join(list(coin.get("chainCirculating", {}).keys())[:3])
            if len(coin.get("chainCirculating", {})) > 3:
                chains += "..."
            rows.append([coin.get("name", ""), coin.get("symbol", ""), format_number(mcap), chains])
        print_markdown_table(["Name", "Symbol", "Market Cap", "Chains"], rows)
        return

    table = Table(title="Stablecoins by Market Cap")
    table.add_column("Name", style="cyan", max_width=20)
    table.add_column("Symbol", style="green", max_width=10)
    table.add_column("Market Cap", style="yellow", justify="right")
    table.add_column("Chains", style="dim", max_width=30)

    for coin in sorted_data:
        mcap = coin.get("circulating", {}).get("peggedUSD", 0)
        chains = ", ".join(list(coin.get("chainCirculating", {}).keys())[:5])
        if len(coin.get("chainCirculating", {})) > 5:
            chains += "..."
        table.add_row(coin.get("name", ""), coin.get("symbol", ""), format_number(mcap), chains)

    console.print(table)


@app.command()
def stablecoin(
    asset: str = typer.Argument(..., help="Stablecoin ID or symbol"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details for a specific stablecoin including chain breakdown."""
    client = get_client()

    if not asset.isdigit():
        stables = client.list_stablecoins()
        for s in stables:
            if (
                s.get("symbol", "").lower() == asset.lower()
                or s.get("name", "").lower() == asset.lower()
            ):
                asset = str(s.get("id"))
                break
        else:
            console.print(f"[red]Stablecoin '{asset}' not found[/]")
            raise typer.Exit(1)

    data = client.get_stablecoin(asset)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('name', 'Unknown')}[/] ({data.get('symbol', '')})")
    console.print(f"[dim]ID: {data.get('id')} | Gecko ID: {data.get('gecko_id', 'N/A')}[/]\n")

    chains = data.get("chainBalances", {})
    table = Table(title="Chain Breakdown")
    table.add_column("Chain", style="cyan")
    table.add_column("Circulating", style="yellow", justify="right")

    sorted_chains = []
    for chain_name, chain_data in chains.items():
        tokens = chain_data.get("tokens", [])
        if tokens:
            latest = tokens[-1] if tokens else {}
            amount = latest.get("circulating", {}).get("peggedUSD", 0)
            sorted_chains.append((chain_name, amount))

    for chain_name, amount in sorted(sorted_chains, key=lambda x: x[1], reverse=True)[:20]:
        table.add_row(chain_name, format_number(amount))

    console.print(table)


@app.command("stablecoin-flows")
def stablecoin_flows(
    chain: str = typer.Argument(..., help="Chain name (e.g., ethereum, arbitrum)"),
    days: int = typer.Option(30, "--days", "-d", help="Number of days to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Show stablecoin inflows/outflows for a chain."""
    client = get_client()
    data = client.get_stablecoin_charts(chain)

    if json_output:
        print(json.dumps(data[-days:] if len(data) > days else data, indent=2))
        return

    if not data:
        console.print(f"[yellow]No stablecoin data for chain '{chain}'[/]")
        raise typer.Exit()

    recent = data[-days:] if len(data) > days else data
    if len(recent) < 2:
        console.print("[yellow]Not enough data points to calculate flows[/]")
        raise typer.Exit()

    first = recent[0].get("totalCirculating", {}).get("peggedUSD", 0)
    last = recent[-1].get("totalCirculating", {}).get("peggedUSD", 0)
    change = last - first
    pct = (change / first * 100) if first > 0 else 0

    color = "green" if change >= 0 else "red"
    direction = "inflow" if change >= 0 else "outflow"

    console.print(f"\n[bold]Stablecoin Flows: {chain}[/] (last {len(recent)} days)\n")
    console.print(f"Start: {format_number(first)}")
    console.print(f"End:   {format_number(last)}")
    console.print(f"[{color}]Net {direction}: {format_number(abs(change))} ({pct:+.2f}%)[/]")


@app.command()
def protocols(
    category: str = typer.Option(None, "--category", "-c", help="Filter by category"),
    chain: str = typer.Option(None, "--chain", help="Filter by chain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List DeFi protocols by TVL."""
    client = get_client()
    data = client.list_protocols()

    if category:
        data = [p for p in data if p.get("category", "").lower() == category.lower()]
    if chain:
        data = [p for p in data if chain.lower() in [c.lower() for c in p.get("chains", [])]]

    data = sorted(data, key=lambda x: x.get("tvl") or 0, reverse=True)[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for p in data:
            chains_str = ", ".join(p.get("chains", [])[:3])
            if len(p.get("chains", [])) > 3:
                chains_str += "..."
            rows.append(
                [
                    p.get("name", ""),
                    p.get("category", ""),
                    format_number(p.get("tvl", 0)),
                    chains_str,
                ]
            )
        print_markdown_table(["Name", "Category", "TVL", "Chains"], rows)
        return

    table = Table(title="DeFi Protocols by TVL")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Category", style="green", max_width=15)
    table.add_column("TVL", style="yellow", justify="right")
    table.add_column("Chains", style="dim", max_width=25)

    for p in data:
        chains_str = ", ".join(p.get("chains", [])[:4])
        if len(p.get("chains", [])) > 4:
            chains_str += "..."
        table.add_row(
            p.get("name", ""), p.get("category", ""), format_number(p.get("tvl", 0)), chains_str
        )

    console.print(table)


@app.command()
def protocol(
    slug: str = typer.Argument(
        ...,
        help=(
            "TVL protocol slug (e.g., aave, uniswap). For perps venues use "
            "derivatives-summary instead, e.g. defillama derivatives-summary hyperliquid."
        ),
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get protocol details including historical TVL."""
    if _looks_like_perps(slug):
        console.print(
            "[red]This looks like a perpetuals venue. Use: "
            f"defillama derivatives-summary {slug} --json[/]"
        )
        raise typer.Exit(2)
    if slug in {"trade-xyz", "trade.xyz"}:
        console.print(
            "[red]No DefiLlama TVL protocol is known for this slug. Check the canonical "
            "slug with `defillama protocols --json` before calling protocol details.[/]"
        )
        raise typer.Exit(2)

    client = get_client()
    data = client.get_protocol(slug)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('name', slug)}[/]")
    console.print(f"[dim]{data.get('description', 'No description')[:100]}[/]\n")

    console.print(f"Category: [green]{data.get('category', 'Unknown')}[/]")
    console.print(f"TVL: [yellow]{format_number(data.get('tvl', 0))}[/]")

    chains = data.get("chains", [])
    if chains:
        console.print(f"Chains: {', '.join(chains[:10])}")

    chain_tvls = data.get("chainTvls", {})
    if chain_tvls:
        console.print("\n[bold]TVL by Chain:[/]")
        table = Table()
        table.add_column("Chain", style="cyan")
        table.add_column("TVL", style="yellow", justify="right")

        sorted_chains = sorted(
            [(k, v) for k, v in chain_tvls.items() if isinstance(v, (int, float))],
            key=lambda x: x[1],
            reverse=True,
        )[:10]
        for chain_name, tvl in sorted_chains:
            table.add_row(chain_name, format_number(tvl))

        console.print(table)


@app.command()
def chains(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List all chains by TVL."""
    client = get_client()
    data = client.list_chains()

    data = sorted(data, key=lambda x: x.get("tvl") or 0, reverse=True)[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = [
            [c.get("name", ""), format_number(c.get("tvl", 0)), c.get("tokenSymbol", "")]
            for c in data
        ]
        print_markdown_table(["Chain", "TVL", "Token"], rows)
        return

    table = Table(title="Chains by TVL")
    table.add_column("Chain", style="cyan")
    table.add_column("TVL", style="yellow", justify="right")
    table.add_column("Token Symbol", style="green")

    for c in data:
        table.add_row(c.get("name", ""), format_number(c.get("tvl", 0)), c.get("tokenSymbol", ""))

    console.print(table)


@app.command("chain-tvl")
def chain_tvl(
    chain: str = typer.Argument(..., help="Chain name"),
    days: int = typer.Option(30, "--days", "-d", help="Number of days to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Historical TVL for a chain."""
    client = get_client()
    data = client.get_chain_tvl(chain)

    if json_output:
        print(json.dumps(data[-days:] if len(data) > days else data, indent=2))
        return

    if not data:
        console.print(f"[yellow]No TVL data for chain '{chain}'[/]")
        raise typer.Exit()

    recent = data[-days:] if len(data) > days else data
    if len(recent) < 2:
        console.print("[yellow]Not enough data points[/]")
        raise typer.Exit()

    first = recent[0].get("tvl", 0)
    last = recent[-1].get("tvl", 0)
    change = last - first
    pct = (change / first * 100) if first > 0 else 0
    color = "green" if change >= 0 else "red"

    console.print(f"\n[bold]TVL History: {chain}[/] (last {len(recent)} days)\n")
    console.print(f"Start: {format_number(first)}")
    console.print(f"End:   {format_number(last)}")
    console.print(f"[{color}]Change: {format_number(abs(change))} ({pct:+.2f}%)[/]")


@app.command("dex-volume")
def dex_volume(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """DEX trading volumes."""
    client = get_client()
    data = client.get_dex_volumes(chain)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    protocols = data.get("protocols", [])
    protocols = sorted(protocols, key=lambda x: x.get("total24h") or 0, reverse=True)[:limit]

    if markdown:
        rows = []
        for p in protocols:
            vol_24h = p.get("total24h", 0) or 0
            vol_7d = p.get("total7d", 0) or 0
            change = p.get("change_1d", 0) or 0
            rows.append(
                [
                    p.get("name", ""),
                    format_number(vol_24h),
                    format_number(vol_7d),
                    f"{change:+.1f}%",
                ]
            )
        print_markdown_table(["Protocol", "24h Volume", "7d Volume", "Change"], rows)
        total_24h = data.get("total24h", 0)
        if total_24h:
            print(f"\nTotal 24h Volume: {format_number(total_24h)}")
        return

    table = Table(title=f"DEX Volumes{f' ({chain})' if chain else ''}")
    table.add_column("Protocol", style="cyan", max_width=25)
    table.add_column("24h Volume", style="yellow", justify="right")
    table.add_column("7d Volume", style="green", justify="right")
    table.add_column("Change", style="dim", justify="right")

    for p in protocols:
        vol_24h = p.get("total24h", 0) or 0
        vol_7d = p.get("total7d", 0) or 0
        change = p.get("change_1d", 0) or 0
        change_color = "green" if change >= 0 else "red"
        table.add_row(
            p.get("name", ""),
            format_number(vol_24h),
            format_number(vol_7d),
            f"[{change_color}]{change:+.1f}%[/]",
        )

    console.print(table)

    total_24h = data.get("total24h", 0)
    if total_24h:
        console.print(f"\n[bold]Total 24h Volume: {format_number(total_24h)}[/]")


@app.command("derivatives-volume")
def derivatives_volume(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Perpetual futures venue volumes.

    Use this for perps venues such as Hyperliquid, Lighter, GMX, dYdX, Drift, and Vertex.
    These venues are under DefiLlama derivatives endpoints, not DEX volume endpoints.
    """
    client = get_client()
    data = client.get_derivatives_volumes(chain)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    protocols = data.get("protocols", [])
    protocols = sorted(protocols, key=lambda x: x.get("total24h") or 0, reverse=True)[:limit]

    if markdown:
        rows = []
        for p in protocols:
            vol_24h = p.get("total24h", 0) or 0
            vol_7d = p.get("total7d", 0) or 0
            change = p.get("change_1d", 0) or 0
            rows.append(
                [
                    p.get("name", ""),
                    format_number(vol_24h),
                    format_number(vol_7d),
                    f"{change:+.1f}%",
                ]
            )
        print_markdown_table(["Venue", "24h Volume", "7d Volume", "Change"], rows)
        total_24h = data.get("total24h", 0)
        if total_24h:
            print(f"\nTotal 24h Derivatives Volume: {format_number(total_24h)}")
        return

    table = Table(title=f"Derivatives Volumes{f' ({chain})' if chain else ''}")
    table.add_column("Venue", style="cyan", max_width=25)
    table.add_column("24h Volume", style="yellow", justify="right")
    table.add_column("7d Volume", style="green", justify="right")
    table.add_column("Change", style="dim", justify="right")

    for p in protocols:
        change = p.get("change_1d", 0) or 0
        change_color = "green" if change >= 0 else "red"
        table.add_row(
            p.get("name", ""),
            format_number(p.get("total24h", 0) or 0),
            format_number(p.get("total7d", 0) or 0),
            f"[{change_color}]{change:+.1f}%[/]",
        )

    console.print(table)

    total_24h = data.get("total24h", 0)
    if total_24h:
        console.print(f"\n[bold]Total 24h Derivatives Volume: {format_number(total_24h)}[/]")


@app.command("derivatives-summary")
def derivatives_summary(
    protocol: str = typer.Argument(
        ..., help="Derivatives protocol slug, e.g. hyperliquid, lighter, gmx-v2, dydx-v4"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Volume details for a specific perpetual futures venue."""
    client = get_client()
    data = client.get_derivatives_summary(protocol)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('name', protocol)}[/] Derivatives\n")
    console.print(f"24h Volume: [yellow]{format_number(data.get('total24h', 0) or 0)}[/]")
    console.print(f"7d Volume: [green]{format_number(data.get('total7d', 0) or 0)}[/]")


@app.command("open-interest")
def open_interest(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Perpetual futures open interest overview.

    Uses DefiLlama's kebab-case open-interest endpoint. Do not use camelCase
    /overview/openInterest.
    """
    client = get_client()
    data = client.get_open_interest_overview(chain)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold]Open Interest{f' ({chain})' if chain else ''}[/]\n")
    total_24h = data.get("total24h")
    total_30d = data.get("total30d")
    if total_24h:
        console.print(f"24h: [yellow]{format_number(total_24h)}[/]")
    if total_30d:
        console.print(f"30d: [green]{format_number(total_30d)}[/]")
    chains = data.get("allChains", [])
    if chains:
        console.print(f"Chains: {', '.join(chains[:10])}")


@app.command("open-interest-summary")
def open_interest_summary(
    protocol: str = typer.Argument(
        ..., help="Open-interest protocol slug, e.g. hyperliquid, lighter, dydx-v4"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Open interest details for a specific perpetual futures venue."""
    client = get_client()
    data = client.get_open_interest_summary(protocol)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('name', protocol)}[/] Open Interest\n")
    console.print(f"Category: [green]{data.get('category', 'Unknown')}[/]")
    console.print(f"Chains: {', '.join(data.get('chains', [])[:10])}")


@app.command()
def bridges(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Bridge volumes and statistics."""
    client = get_client()

    if chain:
        data = client.get_bridge_volumes(chain)
        if json_output:
            print(json.dumps(data, indent=2))
            return
        console.print(f"[bold]Bridge volumes for {chain}[/]")
        if isinstance(data, list):
            for item in data[:limit]:
                console.print(f"  {item}")
        return

    data = client.list_bridges()

    if json_output:
        print(json.dumps(data[:limit], indent=2))
        return

    sorted_bridges = sorted(data, key=lambda x: x.get("lastDailyVolume", 0) or 0, reverse=True)[
        :limit
    ]

    if markdown:
        rows = []
        for b in sorted_bridges:
            vol = b.get("lastDailyVolume", 0) or 0
            chains_list = b.get("chains", [])
            chains_str = ", ".join(chains_list[:3])
            if len(chains_list) > 3:
                chains_str += "..."
            rows.append([b.get("displayName", b.get("name", "")), format_number(vol), chains_str])
        print_markdown_table(["Name", "24h Volume", "Chains"], rows)
        return

    table = Table(title="Bridges")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("24h Volume", style="yellow", justify="right")
    table.add_column("Chains", style="dim", max_width=30)

    for b in sorted_bridges:
        vol = b.get("lastDailyVolume", 0) or 0
        chains_list = b.get("chains", [])
        chains_str = ", ".join(chains_list[:5])
        if len(chains_list) > 5:
            chains_str += "..."
        table.add_row(b.get("displayName", b.get("name", "")), format_number(vol), chains_str)

    console.print(table)


@app.command()
def fees(
    chain: str = typer.Option(None, "--chain", "-c", help="Filter by chain"),
    protocol: str = typer.Option(None, "--protocol", "-p", help="Get fees for specific protocol"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Protocol fees and revenue."""
    client = get_client()

    if protocol:
        data = client.get_protocol_fees(protocol)
        if json_output:
            print(json.dumps(data, indent=2))
            return

        if markdown:
            print(f"**{data.get('name', protocol)}** Fees\n")
            print(f"- 24h Fees: {format_number(data.get('total24h', 0) or 0)}")
            print(f"- All-time Revenue: {format_number(data.get('totalAllTime', 0) or 0)}")
            return

        console.print(f"\n[bold cyan]{data.get('name', protocol)}[/] Fees\n")
        console.print(f"24h Fees: [yellow]{format_number(data.get('total24h', 0) or 0)}[/]")
        console.print(f"24h Revenue: [green]{format_number(data.get('totalAllTime', 0) or 0)}[/]")
        return

    data = client.get_fees(chain)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    protocols = data.get("protocols", [])
    protocols = sorted(protocols, key=lambda x: x.get("total24h") or 0, reverse=True)[:limit]

    if markdown:
        rows = [
            [p.get("name", ""), format_number(p.get("total24h", 0) or 0), p.get("category", "")]
            for p in protocols
        ]
        print_markdown_table(["Protocol", "24h Fees", "Category"], rows)
        return

    table = Table(title=f"Protocol Fees{f' ({chain})' if chain else ''}")
    table.add_column("Protocol", style="cyan", max_width=25)
    table.add_column("24h Fees", style="yellow", justify="right")
    table.add_column("Category", style="green", max_width=15)

    for p in protocols:
        fees_24h = p.get("total24h", 0) or 0
        table.add_row(p.get("name", ""), format_number(fees_24h), p.get("category", ""))

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /stablecoins)"),
    params: str = typer.Option(None, "--params", "-p", help="Query params as key=value,key=value"),
    pro: bool = typer.Option(False, "--pro", help="Use pro API endpoint"),
    base: str = typer.Option(
        None, "--base", "-b", help="Base URL (main, stablecoins, bridges, coins). Default: main"
    ),
):
    """Make a raw API call. Params as key=value,key=value.

    Prefer typed commands when available:
      - defillama derivatives-volume, not raw /overview/derivatives
      - defillama derivatives-summary hyperliquid, not protocol hyperliquid
      - defillama raw /overview/open-interest, not /overview/openInterest
      - defillama open-interest-summary hyperliquid, not raw /summary/openInterest/hyperliquid

    Base URLs:
      main: https://api.llama.fi (default)
      stablecoins: https://stablecoins.llama.fi
      bridges: https://bridges.llama.fi
      coins: https://coins.llama.fi
    """

    client = get_client()

    query_params = None
    if params:
        query_params = {}
        for pair in params.split(","):
            if "=" in pair:
                k, v = pair.split("=", 1)
                query_params[k.strip()] = v.strip()

    base_url = None
    if base:
        base_map = {
            "main": client.base_url,
            "stablecoins": client.stablecoins_url,
            "bridges": client.bridges_url,
            "coins": "https://coins.llama.fi",
        }
        base_url = base_map.get(base.lower())
        if not base_url:
            console.print(f"[red]Unknown base: {base}. Use: main, stablecoins, bridges, coins[/]")
            raise typer.Exit(1)

    endpoint_lower = endpoint.lower()
    if endpoint == "/overview/openInterest":
        console.print(
            "[red]Use /overview/open-interest, not /overview/openInterest. "
            "Prefer: defillama open-interest --json[/]"
        )
        raise typer.Exit(2)
    if endpoint_lower.startswith("/overview/derivatives") and pro:
        console.print(
            "[red]/overview/derivatives is a public main API endpoint; do not pass --pro. "
            "Prefer: defillama derivatives-volume --json[/]"
        )
        raise typer.Exit(2)
    if endpoint_lower.startswith("/summary/open-interest"):
        console.print(
            "[red]Prefer the typed command: defillama open-interest-summary <protocol> --json[/]"
        )
        raise typer.Exit(2)
    if endpoint.startswith("/summary/openInterest"):
        console.print(
            "[red]Use /summary/open-interest/<protocol>, not /summary/openInterest/<protocol>. "
            "Prefer: defillama open-interest-summary <protocol> --json[/]"
        )
        raise typer.Exit(2)

    try:
        data = client._request(endpoint, params=query_params, pro=pro, base=base_url)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
