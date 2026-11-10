"""CLI for DeBank API."""

import json

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="debank", help="DeBank CLI for DeFi wallet data and protocol positions")


@app.command("health")
def health():
    """Assert debank connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_chain_list()
        payload = {"ok": True, "tool": "debank", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "debank", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import DeBankClient

    return DeBankClient()


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


def format_amount(value: float | None, decimals: int = 4) -> str:
    """Format token amounts."""
    if value is None:
        return "N/A"
    if value >= 1e9:
        return f"{value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"{value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"{value / 1e3:.{decimals}f}K"
    return f"{value:.{decimals}f}"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


# === User Commands ===


@app.command()
def balance(
    address: str = typer.Argument(..., help="Wallet address"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get total balance across all chains."""
    client = get_client()
    data = client.get_user_total_balance(address)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    total = data.get("total_usd_value", 0)
    chain_list = data.get("chain_list", [])

    if markdown:
        print(f"**Total Balance:** {format_number(total)}\n")
        if chain_list:
            rows = []
            for c in sorted(chain_list, key=lambda x: x.get("usd_value", 0), reverse=True):
                if c.get("usd_value", 0) > 0:
                    rows.append(
                        [c.get("id", ""), c.get("name", ""), format_number(c.get("usd_value"))]
                    )
            if rows:
                print_markdown_table(["Chain", "Name", "Value"], rows)
        return

    console.print(f"\n[bold]Total Balance:[/] [green]{format_number(total)}[/]\n")

    if chain_list:
        table = Table(title="Balance by Chain")
        table.add_column("Chain", style="cyan")
        table.add_column("Name", style="white")
        table.add_column("Value", style="green", justify="right")

        for c in sorted(chain_list, key=lambda x: x.get("usd_value", 0), reverse=True):
            if c.get("usd_value", 0) > 0:
                table.add_row(c.get("id", ""), c.get("name", ""), format_number(c.get("usd_value")))

        console.print(table)


@app.command()
def tokens(
    address: str = typer.Argument(..., help="Wallet address"),
    chain: str = typer.Option(
        None, "--chain", "-c", help="Chain ID (eth, bsc, etc). If omitted, shows all chains"
    ),
    limit: int = typer.Option(20, "--limit", "-n", help="Max tokens to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get token balances for a wallet."""
    client = get_client()

    if chain:
        data = client.get_user_token_list(address, chain)
    else:
        data = client.get_user_all_token_list(address)

    # Sort by USD value
    data = sorted(
        data, key=lambda x: (x.get("price", 0) or 0) * (x.get("amount", 0) or 0), reverse=True
    )
    data = data[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for t in data:
            symbol = t.get("optimized_symbol") or t.get("symbol") or "?"
            amount = t.get("amount", 0)
            price = t.get("price", 0)
            usd_value = (price or 0) * (amount or 0)
            rows.append(
                [
                    t.get("chain", ""),
                    symbol,
                    format_amount(amount),
                    format_number(price),
                    format_number(usd_value),
                ]
            )
        print_markdown_table(["Chain", "Token", "Amount", "Price", "Value"], rows)
        return

    table = Table(title=f"Tokens{f' on {chain}' if chain else ' (all chains)'}")
    table.add_column("Chain", style="dim")
    table.add_column("Token", style="cyan")
    table.add_column("Amount", style="white", justify="right")
    table.add_column("Price", style="yellow", justify="right")
    table.add_column("Value", style="green", justify="right")

    for t in data:
        symbol = t.get("optimized_symbol") or t.get("symbol") or "?"
        amount = t.get("amount", 0)
        price = t.get("price", 0)
        usd_value = (price or 0) * (amount or 0)
        table.add_row(
            t.get("chain", ""),
            symbol,
            format_amount(amount),
            format_number(price),
            format_number(usd_value),
        )

    console.print(table)


@app.command()
def protocols(
    address: str = typer.Argument(..., help="Wallet address"),
    chain: str = typer.Option(
        None, "--chain", "-c", help="Chain ID (eth, bsc, etc). If omitted, shows all chains"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get DeFi protocol positions for a wallet."""
    client = get_client()

    if chain:
        data = client.get_user_simple_protocol_list(address, chain)
    else:
        data = client.get_user_all_simple_protocol_list(address)

    # Sort by net value
    data = sorted(data, key=lambda x: x.get("net_usd_value", 0) or 0, reverse=True)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No protocol positions found[/]")
        return

    if markdown:
        rows = []
        for p in data:
            rows.append(
                [
                    p.get("chain", ""),
                    p.get("name", ""),
                    format_number(p.get("net_usd_value")),
                    format_number(p.get("asset_usd_value")),
                    format_number(p.get("debt_usd_value")),
                ]
            )
        print_markdown_table(["Chain", "Protocol", "Net Value", "Assets", "Debt"], rows)
        return

    table = Table(title=f"DeFi Positions{f' on {chain}' if chain else ''}")
    table.add_column("Chain", style="dim")
    table.add_column("Protocol", style="cyan")
    table.add_column("Net Value", style="green", justify="right")
    table.add_column("Assets", style="white", justify="right")
    table.add_column("Debt", style="red", justify="right")

    for p in data:
        table.add_row(
            p.get("chain", ""),
            p.get("name", ""),
            format_number(p.get("net_usd_value")),
            format_number(p.get("asset_usd_value")),
            format_number(p.get("debt_usd_value")),
        )

    console.print(table)


@app.command()
def positions(
    address: str = typer.Argument(..., help="Wallet address"),
    chain: str = typer.Option(None, "--chain", "-c", help="Chain ID. If omitted, shows all chains"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get detailed DeFi positions (complex protocol list)."""
    client = get_client()

    if chain:
        data = client.get_user_complex_protocol_list(address, chain)
    else:
        data = client.get_user_all_complex_protocol_list(address)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No positions found[/]")
        return

    for protocol in data:
        name = protocol.get("name", "Unknown")
        chain_id = protocol.get("chain", "")
        portfolio_items = protocol.get("portfolio_item_list", [])

        if markdown:
            print(f"\n## {name} ({chain_id})\n")
        else:
            console.print(f"\n[bold cyan]{name}[/] [dim]({chain_id})[/]")

        for item in portfolio_items:
            item_name = item.get("name", "Position")
            detail = item.get("detail", {})
            supply = detail.get("supply_token_list", [])
            reward = detail.get("reward_token_list", [])
            borrow = detail.get("borrow_token_list", [])

            if markdown:
                print(f"\n**{item_name}**")
                if supply:
                    for t in supply:
                        sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                        amt = t.get("amount", 0)
                        print(f"- Supply: {format_amount(amt)} {sym}")
                if reward:
                    for t in reward:
                        sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                        amt = t.get("amount", 0)
                        print(f"- Reward: {format_amount(amt)} {sym}")
                if borrow:
                    for t in borrow:
                        sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                        amt = t.get("amount", 0)
                        print(f"- Borrow: {format_amount(amt)} {sym}")
            else:
                console.print(f"  [white]{item_name}[/]")
                if supply:
                    for t in supply:
                        sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                        amt = t.get("amount", 0)
                        console.print(f"    [green]+ Supply:[/] {format_amount(amt)} {sym}")
                if reward:
                    for t in reward:
                        sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                        amt = t.get("amount", 0)
                        console.print(f"    [yellow]★ Reward:[/] {format_amount(amt)} {sym}")
                if borrow:
                    for t in borrow:
                        sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                        amt = t.get("amount", 0)
                        console.print(f"    [red]- Borrow:[/] {format_amount(amt)} {sym}")


@app.command()
def protocol(
    address: str = typer.Argument(..., help="Wallet address"),
    protocol_id: str = typer.Argument(..., help="Protocol ID (e.g., uniswap, aave, curve)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get positions for a specific protocol."""
    client = get_client()
    data = client.get_user_protocol(address, protocol_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    name = data.get("name", protocol_id)
    chain = data.get("chain", "")
    items = data.get("portfolio_item_list", [])

    console.print(f"\n[bold cyan]{name}[/] [dim]({chain})[/]")

    if not items:
        console.print("[yellow]No positions in this protocol[/]")
        return

    for item in items:
        item_name = item.get("name", "Position")
        stats = item.get("stats", {})
        net_usd = stats.get("net_usd_value", 0)
        console.print(f"\n  [white]{item_name}[/] - {format_number(net_usd)}")

        detail = item.get("detail", {})
        for key in ["supply_token_list", "reward_token_list", "borrow_token_list"]:
            tokens_list = detail.get(key, [])
            if tokens_list:
                label = key.replace("_token_list", "").title()
                for t in tokens_list:
                    sym = t.get("optimized_symbol") or t.get("symbol") or "?"
                    amt = t.get("amount", 0)
                    console.print(f"    {label}: {format_amount(amt)} {sym}")


# === Chain Commands ===


@app.command()
def chains(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List supported chains."""
    client = get_client()
    data = client.get_chain_list()

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for c in data:
            rows.append([c.get("id", ""), c.get("name", ""), c.get("native_token_id", "")])
        print_markdown_table(["ID", "Name", "Native Token"], rows)
        return

    table = Table(title="Supported Chains")
    table.add_column("ID", style="cyan")
    table.add_column("Name", style="white")
    table.add_column("Native Token", style="yellow")

    for c in data:
        table.add_row(c.get("id", ""), c.get("name", ""), c.get("native_token_id", ""))

    console.print(table)


# === Protocol Info Commands ===


@app.command("protocol-info")
def protocol_info(
    protocol_id: str = typer.Argument(..., help="Protocol ID (e.g., uniswap, aave)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get info about a protocol."""
    client = get_client()
    data = client.get_protocol(protocol_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    console.print(f"\n[bold cyan]{data.get('name', protocol_id)}[/]")
    console.print(f"ID: {data.get('id', '')}")
    console.print(f"Chain: {data.get('chain', '')}")
    console.print(f"Site: {data.get('site_url', 'N/A')}")
    console.print(f"TVL: {format_number(data.get('tvl'))}")


@app.command("protocol-list")
def protocol_list(
    chain: str = typer.Option("eth", "--chain", "-c", help="Chain ID"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List protocols on a chain."""
    client = get_client()
    data = client.get_protocol_list(chain)

    # Sort by TVL if available
    data = sorted(data, key=lambda x: x.get("tvl", 0) or 0, reverse=True)[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = []
        for p in data:
            rows.append([p.get("id", ""), p.get("name", ""), format_number(p.get("tvl"))])
        print_markdown_table(["ID", "Name", "TVL"], rows)
        return

    table = Table(title=f"Protocols on {chain}")
    table.add_column("ID", style="dim")
    table.add_column("Name", style="cyan")
    table.add_column("TVL", style="green", justify="right")

    for p in data:
        table.add_row(p.get("id", ""), p.get("name", ""), format_number(p.get("tvl")))

    console.print(table)


# === Token Commands ===


@app.command()
def token(
    token_id: str = typer.Argument(..., help="Token address or native token ID"),
    chain: str = typer.Option("eth", "--chain", "-c", help="Chain ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get token info."""
    client = get_client()
    data = client.get_token(chain, token_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    symbol = data.get("optimized_symbol") or data.get("symbol") or "?"
    console.print(f"\n[bold cyan]{data.get('name', 'Unknown')}[/] ({symbol})")
    console.print(f"Chain: {data.get('chain', '')}")
    console.print(f"Address: {data.get('id', '')}")
    console.print(f"Price: {format_number(data.get('price'))}")
    console.print(f"Decimals: {data.get('decimals', 'N/A')}")


# === NFT Commands ===


@app.command()
def nfts(
    address: str = typer.Argument(..., help="Wallet address"),
    chain: str = typer.Option("eth", "--chain", "-c", help="Chain ID"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get NFTs for a wallet."""
    client = get_client()
    data = client.get_user_nft_list(address, chain)
    data = data[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No NFTs found[/]")
        return

    if markdown:
        rows = []
        for n in data:
            rows.append(
                [
                    n.get("contract_name", "")[:20],
                    n.get("name", "")[:30],
                    format_number(n.get("usd_price")),
                ]
            )
        print_markdown_table(["Collection", "Name", "Price"], rows)
        return

    table = Table(title=f"NFTs on {chain}")
    table.add_column("Collection", style="cyan", max_width=20)
    table.add_column("Name", style="white", max_width=30)
    table.add_column("Price", style="green", justify="right")

    for n in data:
        table.add_row(
            n.get("contract_name", "")[:20],
            n.get("name", "")[:30],
            format_number(n.get("usd_price")),
        )

    console.print(table)


# === Raw API ===


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /v1/user/total_balance)"),
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
