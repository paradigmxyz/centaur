"""CLI for Nansen API."""

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="nansen", help="Nansen CLI for blockchain analytics and wallet labels")


@app.command("health")
def health():
    """Assert nansen connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_token_screener(per_page=1)
        payload = {"ok": True, "tool": "nansen", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "nansen", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import NansenClient

    return NansenClient()


def format_number(value: float | None, decimals: int = 2, prefix: str = "$") -> str:
    """Format large numbers with B/M/K suffixes."""
    if value is None:
        return "N/A"
    if value >= 1e12:
        return f"{prefix}{value / 1e12:.{decimals}f}T"
    elif value >= 1e9:
        return f"{prefix}{value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"{prefix}{value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"{prefix}{value / 1e3:.{decimals}f}K"
    return f"{prefix}{value:.{decimals}f}"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def truncate(s: str, max_len: int = 20) -> str:
    """Truncate a string with ellipsis."""
    if len(s) <= max_len:
        return s
    return s[: max_len - 3] + "..."


@app.command()
def labels(
    address: str = typer.Argument(..., help="Wallet address to lookup"),
    chain: str = typer.Option(
        "ethereum", "--chain", "-c", help="Blockchain (ethereum, solana, etc.)"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Nansen labels for a wallet address."""
    client = get_client()

    try:
        data = client.get_address_labels(address, chain=chain)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    labels_list = data.get("Response", [])
    if not labels_list:
        console.print(f"[yellow]No labels found for {address} on {chain}[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for lbl in labels_list:
            rows.append(
                [
                    lbl.get("label", ""),
                    lbl.get("category", ""),
                    lbl.get("fullname", "") or "",
                    truncate(lbl.get("definition", "") or "", 50),
                ]
            )
        print_markdown_table(["Label", "Category", "Entity", "Definition"], rows)
        return

    table = Table(title=f"Labels for {address[:10]}...{address[-6:]}")
    table.add_column("Label", style="cyan")
    table.add_column("Category", style="yellow")
    table.add_column("Entity", style="green", max_width=25)
    table.add_column("Definition", style="dim", max_width=40)

    for lbl in labels_list:
        table.add_row(
            lbl.get("label", ""),
            lbl.get("category", ""),
            lbl.get("fullname", "") or "",
            truncate(lbl.get("definition", "") or "", 40),
        )

    console.print(table)


@app.command()
def balance(
    address: str = typer.Argument(None, help="Wallet address"),
    entity: str = typer.Option(
        None, "--entity", "-e", help="Entity name (e.g., 'Vitalik Buterin')"
    ),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Blockchain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get current token balances for an address or entity."""
    if not address and not entity:
        console.print("[red]Error: Provide either an address or --entity[/]")
        raise typer.Exit(1)

    client = get_client()

    try:
        data = client.get_address_balance(
            address=address, entity_name=entity, chain=chain, per_page=limit
        )
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    balances = response.get("data", []) if isinstance(response, dict) else response
    if not balances:
        console.print("[yellow]No balances found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for bal in balances[:limit]:
            rows.append(
                [
                    bal.get("token_symbol", ""),
                    f"{bal.get('balance', 0):.4f}",
                    format_number(bal.get("value_usd")),
                    bal.get("chain", ""),
                ]
            )
        print_markdown_table(["Token", "Balance", "USD Value", "Chain"], rows)
        return

    title = f"Balances for {entity or address[:10] + '...' + address[-6:]}"
    table = Table(title=title)
    table.add_column("Token", style="cyan")
    table.add_column("Balance", style="yellow", justify="right")
    table.add_column("USD Value", style="green", justify="right")
    table.add_column("Chain", style="dim")

    for bal in balances[:limit]:
        table.add_row(
            bal.get("token_symbol", ""),
            f"{bal.get('balance', 0):.4f}",
            format_number(bal.get("value_usd")),
            bal.get("chain", ""),
        )

    console.print(table)


@app.command("smart-money")
def smart_money_holdings(
    chains: str = typer.Option("ethereum", "--chains", "-c", help="Chains (comma-separated)"),
    labels_filter: str = typer.Option(
        None, "--labels", "-l", help="Labels filter (comma-separated: Fund, Smart Trader, etc.)"
    ),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Smart Money token holdings."""
    client = get_client()
    chain_list = [c.strip() for c in chains.split(",")]
    label_list = [lbl.strip() for lbl in labels_filter.split(",")] if labels_filter else None

    try:
        data = client.get_smart_money_holdings(chains=chain_list, labels=label_list, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    holdings = response.get("data", []) if isinstance(response, dict) else response
    if not holdings:
        console.print("[yellow]No Smart Money holdings found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for h in holdings[:limit]:
            rows.append(
                [
                    h.get("token_symbol", ""),
                    str(h.get("smart_money_count", "")),
                    format_number(h.get("total_value_usd")),
                    h.get("chain", ""),
                ]
            )
        print_markdown_table(["Token", "SM Holders", "Total Value", "Chain"], rows)
        return

    table = Table(title="Smart Money Holdings")
    table.add_column("Token", style="cyan")
    table.add_column("SM Holders", style="yellow", justify="right")
    table.add_column("Total Value", style="green", justify="right")
    table.add_column("Chain", style="dim")

    for h in holdings[:limit]:
        table.add_row(
            h.get("token_symbol", ""),
            str(h.get("smart_money_count", "")),
            format_number(h.get("total_value_usd")),
            h.get("chain", ""),
        )

    console.print(table)


@app.command()
def netflows(
    chains: str = typer.Option("ethereum", "--chains", "-c", help="Chains (comma-separated)"),
    labels_filter: str = typer.Option(None, "--labels", "-l", help="Labels filter"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Smart Money net flows (what they're buying/selling)."""
    client = get_client()
    chain_list = [c.strip() for c in chains.split(",")]
    label_list = [lbl.strip() for lbl in labels_filter.split(",")] if labels_filter else None

    try:
        data = client.get_smart_money_netflows(chains=chain_list, labels=label_list, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    flows = response.get("data", []) if isinstance(response, dict) else response
    if not flows:
        console.print("[yellow]No netflows found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for f in flows[:limit]:
            netflow = f.get("netflow_usd", 0)
            sign = "+" if netflow >= 0 else ""
            rows.append(
                [
                    f.get("token_symbol", ""),
                    f"{sign}{format_number(netflow)}",
                    str(f.get("buyers", 0)),
                    str(f.get("sellers", 0)),
                    f.get("chain", ""),
                ]
            )
        print_markdown_table(["Token", "Net Flow", "Buyers", "Sellers", "Chain"], rows)
        return

    table = Table(title="Smart Money Net Flows")
    table.add_column("Token", style="cyan")
    table.add_column("Net Flow", justify="right")
    table.add_column("Buyers", style="green", justify="right")
    table.add_column("Sellers", style="red", justify="right")
    table.add_column("Chain", style="dim")

    for f in flows[:limit]:
        netflow = f.get("netflow_usd", 0)
        color = "green" if netflow >= 0 else "red"
        sign = "+" if netflow >= 0 else ""
        table.add_row(
            f.get("token_symbol", ""),
            f"[{color}]{sign}{format_number(netflow)}[/]",
            str(f.get("buyers", 0)),
            str(f.get("sellers", 0)),
            f.get("chain", ""),
        )

    console.print(table)


@app.command("dex-trades")
def smart_money_dex_trades(
    chains: str = typer.Option("ethereum", "--chains", "-c", help="Chains (comma-separated)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Smart Money DEX trades in last 24h."""
    client = get_client()
    chain_list = [c.strip() for c in chains.split(",")]

    try:
        data = client.get_smart_money_dex_trades(chains=chain_list, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    trades = response.get("data", []) if isinstance(response, dict) else response
    if not trades:
        console.print("[yellow]No DEX trades found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for t in trades[:limit]:
            rows.append(
                [
                    t.get("trader_address", "")[:10] + "...",
                    t.get("action", ""),
                    t.get("token_symbol", ""),
                    format_number(t.get("value_usd")),
                    t.get("chain", ""),
                ]
            )
        print_markdown_table(["Trader", "Action", "Token", "Value", "Chain"], rows)
        return

    table = Table(title="Smart Money DEX Trades (24h)")
    table.add_column("Trader", style="dim")
    table.add_column("Action", style="yellow")
    table.add_column("Token", style="cyan")
    table.add_column("Value", style="green", justify="right")
    table.add_column("Chain", style="dim")

    for t in trades[:limit]:
        addr = t.get("trader_address", "")
        action = t.get("action", "")
        action_color = "green" if action.lower() == "buy" else "red"
        table.add_row(
            f"{addr[:6]}...{addr[-4:]}" if len(addr) > 10 else addr,
            f"[{action_color}]{action}[/]",
            t.get("token_symbol", ""),
            format_number(t.get("value_usd")),
            t.get("chain", ""),
        )

    console.print(table)


@app.command()
def screener(
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Blockchain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Token screener - discover tokens with Smart Money activity."""
    client = get_client()

    try:
        data = client.get_token_screener(chain=chain, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    tokens = response.get("data", []) if isinstance(response, dict) else response
    if not tokens:
        console.print("[yellow]No tokens found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for t in tokens[:limit]:
            rows.append(
                [
                    t.get("token_symbol", ""),
                    t.get("token_name", ""),
                    format_number(t.get("market_cap")),
                    str(t.get("smart_money_holders", 0)),
                ]
            )
        print_markdown_table(["Symbol", "Name", "Market Cap", "SM Holders"], rows)
        return

    table = Table(title=f"Token Screener ({chain})")
    table.add_column("Symbol", style="cyan")
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Market Cap", style="green", justify="right")
    table.add_column("SM Holders", style="yellow", justify="right")

    for t in tokens[:limit]:
        table.add_row(
            t.get("token_symbol", ""),
            truncate(t.get("token_name", ""), 25),
            format_number(t.get("market_cap")),
            str(t.get("smart_money_holders", 0)),
        )

    console.print(table)


@app.command()
def holders(
    token: str = typer.Argument(..., help="Token contract address"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Blockchain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get top holders for a token."""
    client = get_client()

    try:
        data = client.get_token_holders(token, chain=chain, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    holders_list = response.get("data", []) if isinstance(response, dict) else response
    if not holders_list:
        console.print("[yellow]No holders found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for h in holders_list[:limit]:
            addr = h.get("address", "")
            rows.append(
                [
                    h.get("entity_name", "") or f"{addr[:6]}...{addr[-4:]}",
                    f"{h.get('balance', 0):.2f}",
                    format_number(h.get("value_usd")),
                    ", ".join(h.get("labels", [])[:2]) if h.get("labels") else "",
                ]
            )
        print_markdown_table(["Holder", "Balance", "Value", "Labels"], rows)
        return

    table = Table(title="Top Holders")
    table.add_column("Holder", style="cyan", max_width=25)
    table.add_column("Balance", style="yellow", justify="right")
    table.add_column("Value", style="green", justify="right")
    table.add_column("Labels", style="dim", max_width=30)

    for h in holders_list[:limit]:
        addr = h.get("address", "")
        name = h.get("entity_name", "") or f"{addr[:6]}...{addr[-4:]}"
        labels_str = ", ".join(h.get("labels", [])[:2]) if h.get("labels") else ""
        table.add_row(
            truncate(name, 25),
            f"{h.get('balance', 0):.2f}",
            format_number(h.get("value_usd")),
            truncate(labels_str, 30),
        )

    console.print(table)


@app.command()
def flows(
    token: str = typer.Argument(..., help="Token contract address"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Blockchain"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get token inflows/outflows by entity type."""
    client = get_client()

    try:
        data = client.get_token_flows(token, chain=chain)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    if not response:
        console.print("[yellow]No flow data found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for entity_type, flow_data in response.items():
            if isinstance(flow_data, dict):
                inflow = flow_data.get("inflow_usd", 0)
                outflow = flow_data.get("outflow_usd", 0)
                net = inflow - outflow
                rows.append(
                    [
                        entity_type,
                        format_number(inflow),
                        format_number(outflow),
                        format_number(net),
                    ]
                )
        print_markdown_table(["Entity Type", "Inflow", "Outflow", "Net"], rows)
        return

    table = Table(title="Token Flows by Entity Type")
    table.add_column("Entity Type", style="cyan")
    table.add_column("Inflow", style="green", justify="right")
    table.add_column("Outflow", style="red", justify="right")
    table.add_column("Net", justify="right")

    for entity_type, flow_data in response.items():
        if isinstance(flow_data, dict):
            inflow = flow_data.get("inflow_usd", 0)
            outflow = flow_data.get("outflow_usd", 0)
            net = inflow - outflow
            net_color = "green" if net >= 0 else "red"
            table.add_row(
                entity_type,
                format_number(inflow),
                format_number(outflow),
                f"[{net_color}]{format_number(net)}[/]",
            )

    console.print(table)


@app.command()
def pnl(
    address: str = typer.Argument(..., help="Wallet address"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Blockchain"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get PnL and trade performance for an address."""
    client = get_client()

    try:
        data = client.get_address_pnl(address, chain=chain)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    if not response:
        console.print("[yellow]No PnL data found[/]")
        raise typer.Exit()

    if markdown:
        print(f"## PnL for {address[:10]}...{address[-6:]}\n")
        if isinstance(response, dict):
            for key, val in response.items():
                if isinstance(val, (int, float)):
                    print(f"- **{key}**: {format_number(val)}")
                else:
                    print(f"- **{key}**: {val}")
        return

    console.print(f"\n[bold]PnL for {address[:10]}...{address[-6:]}[/]\n")
    if isinstance(response, dict):
        for key, val in response.items():
            if isinstance(val, (int, float)):
                color = "green" if val >= 0 else "red"
                console.print(f"  {key}: [{color}]{format_number(val)}[/]")
            else:
                console.print(f"  {key}: {val}")


@app.command("related-wallets")
def related_wallets(
    address: str = typer.Argument(..., help="Wallet address"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Blockchain"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Find wallets related to an address."""
    client = get_client()

    try:
        data = client.get_address_related_wallets(address, chain=chain, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    wallets = response.get("data", []) if isinstance(response, dict) else response
    if not wallets:
        console.print("[yellow]No related wallets found[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for w in wallets[:limit]:
            addr = w.get("address", "")
            rows.append(
                [
                    f"{addr[:6]}...{addr[-4:]}" if len(addr) > 10 else addr,
                    w.get("relation_type", ""),
                    w.get("entity_name", "") or "",
                ]
            )
        print_markdown_table(["Address", "Relation", "Entity"], rows)
        return

    table = Table(title="Related Wallets")
    table.add_column("Address", style="cyan")
    table.add_column("Relation", style="yellow")
    table.add_column("Entity", style="green", max_width=25)

    for w in wallets[:limit]:
        addr = w.get("address", "")
        table.add_row(
            f"{addr[:6]}...{addr[-4:]}" if len(addr) > 10 else addr,
            w.get("relation_type", ""),
            w.get("entity_name", "") or "",
        )

    console.print(table)


@app.command("entity-search")
def entity_search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for an entity by name."""
    client = get_client()

    try:
        data = client.search_entity(query, per_page=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    response = data.get("Response", {})
    entities = response.get("data", []) if isinstance(response, dict) else response
    if not entities:
        console.print(f"[yellow]No entities found for '{query}'[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for e in entities[:limit]:
            rows.append(
                [
                    e.get("entity_name", ""),
                    str(e.get("wallet_count", 0)),
                ]
            )
        print_markdown_table(["Entity Name", "Wallet Count"], rows)
        return

    table = Table(title=f"Entity Search: '{query}'")
    table.add_column("Entity Name", style="cyan")
    table.add_column("Wallet Count", style="yellow", justify="right")

    for e in entities[:limit]:
        table.add_row(
            e.get("entity_name", ""),
            str(e.get("wallet_count", 0)),
        )

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /api/v1/smart-money/holdings)"),
    data: str = typer.Option(None, "--data", "-d", help="JSON request body"),
):
    """Make a raw API call."""
    client = get_client()

    request_data = None
    if data:
        try:
            request_data = json.loads(data)
        except json.JSONDecodeError as e:
            console.print(f"[red]Invalid JSON: {e}[/]")
            raise typer.Exit(1)

    try:
        result = client._request(endpoint, data=request_data)
        print(json.dumps(result, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
