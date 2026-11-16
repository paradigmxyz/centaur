"""CLI for Alchemy blockchain data."""

import json
import sys

import typer
from rich.console import Console
from rich.table import Table

from .client import (
    SUPPORTED_CHAINS,
    AlchemyClient,
    format_gwei,
    format_wei,
)

app = typer.Typer(
    name="alchemy",
    help="Alchemy CLI for blockchain data, token balances, transfers, and prices",
)


@app.command("health")
def health():
    """Assert alchemy connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_block_number()
        payload = {"ok": True, "tool": "alchemy", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "alchemy", "error": str(exc), "details": {}}
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


def format_output(data: list[dict], output_format: str, markdown: bool = False) -> None:
    """Format and print output data."""
    if not data:
        if markdown:
            print("No results.")
        else:
            console.print("[yellow]No results.[/]")
        return

    if output_format == "json":
        print(json.dumps(data, indent=2, default=str), file=sys.stdout)
    elif output_format == "csv":
        if data:
            headers = list(data[0].keys())
            print(",".join(headers))
            for row in data:
                print(",".join(str(row.get(h, "")) for h in headers))
    elif markdown:
        if data:
            headers = list(data[0].keys())
            rows = [[str(row.get(h, "")) for h in headers] for row in data]
            print_markdown_table(headers, rows)
    else:
        table = Table()
        if data:
            for col in data[0].keys():
                table.add_column(str(col), overflow="fold")
            for row in data:
                table.add_row(*[str(v) for v in row.values()])
        console.print(table)


@app.command()
def chains():
    """List supported chains."""
    console.print("[bold]Supported Chains[/]")
    table = Table()
    table.add_column("Alias", style="cyan")
    table.add_column("Chain ID", style="yellow")

    seen = set()
    for alias, chain_id in sorted(SUPPORTED_CHAINS.items(), key=lambda x: x[1]):
        if chain_id not in seen:
            table.add_row(alias, chain_id)
            seen.add(chain_id)
        else:
            table.add_row(f"  ({alias})", "")

    console.print(table)


@app.command()
def balance(
    address: str = typer.Argument(..., help="Wallet address (0x...)"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get native token balance for an address."""
    try:
        with AlchemyClient(chain=chain) as client:
            if not markdown:
                console.print(f"[dim]Fetching balance on {chain}...[/]")
            balance_wei = client.get_balance(address, chain)
            balance_eth = format_wei(balance_wei)

            if output == "json":
                print(
                    json.dumps(
                        {
                            "address": address,
                            "chain": chain,
                            "balance_wei": balance_wei,
                            "balance": balance_eth,
                        }
                    )
                )
            elif markdown:
                print(f"**Address:** `{address}`")
                print(f"**Chain:** {chain}")
                print(f"**Balance:** {balance_eth}")
            else:
                console.print(f"[bold]Address:[/] {address}")
                console.print(f"[bold]Chain:[/] {chain}")
                console.print(f"[bold]Balance:[/] [green]{balance_eth}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("token-balances")
def token_balances(
    address: str = typer.Argument(..., help="Wallet address (0x...)"),
    tokens: str = typer.Option(None, "--tokens", "-t", help="Comma-separated token addresses"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max tokens to display"),
):
    """Get ERC-20 token balances for an address."""
    try:
        with AlchemyClient(chain=chain) as client:
            if not markdown:
                console.print(f"[dim]Fetching token balances on {chain}...[/]")

            token_list = [t.strip() for t in tokens.split(",")] if tokens else None
            result = client.get_token_balances(address, token_list, chain)

            balances = result.get("tokenBalances", [])
            non_zero = []

            for bal in balances[:limit]:
                token_addr = bal.get("contractAddress", "")
                raw_balance = bal.get("tokenBalance", "0x0")

                if raw_balance == "0x0" or raw_balance == "0x" or int(raw_balance, 16) == 0:
                    continue

                try:
                    metadata = client.get_token_metadata(token_addr, chain)
                    symbol = metadata.get("symbol", "???")
                    name = metadata.get("name", "Unknown")
                    decimals = metadata.get("decimals", 18) or 18
                except Exception:
                    symbol = "???"
                    name = "Unknown"
                    decimals = 18

                balance_int = int(raw_balance, 16)
                balance_formatted = format_wei(balance_int, decimals)

                non_zero.append(
                    {
                        "symbol": symbol,
                        "name": name,
                        "balance": balance_formatted,
                        "contract": token_addr[:10] + "..." + token_addr[-6:],
                    }
                )

            if not non_zero:
                if markdown:
                    print("No token balances found.")
                else:
                    console.print("[yellow]No token balances found.[/]")
                return

            format_output(non_zero, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("token-metadata")
def token_metadata(
    token_address: str = typer.Argument(..., help="Token contract address"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get metadata for an ERC-20 token."""
    try:
        with AlchemyClient(chain=chain) as client:
            if not markdown:
                console.print(f"[dim]Fetching token metadata on {chain}...[/]")
            metadata = client.get_token_metadata(token_address, chain)

            if output == "json":
                print(json.dumps(metadata, indent=2))
            elif markdown:
                print(f"**Name:** {metadata.get('name', 'N/A')}")
                print(f"**Symbol:** {metadata.get('symbol', 'N/A')}")
                print(f"**Decimals:** {metadata.get('decimals', 'N/A')}")
                if metadata.get("logo"):
                    print(f"**Logo:** {metadata.get('logo')}")
            else:
                console.print(f"[bold]Name:[/] {metadata.get('name', 'N/A')}")
                console.print(f"[bold]Symbol:[/] [cyan]{metadata.get('symbol', 'N/A')}[/]")
                console.print(f"[bold]Decimals:[/] {metadata.get('decimals', 'N/A')}")
                if metadata.get("logo"):
                    console.print(f"[bold]Logo:[/] {metadata.get('logo')}")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def transfers(
    address: str = typer.Argument(..., help="Wallet address to query transfers for"),
    direction: str = typer.Option("both", "--direction", "-d", help="from, to, or both"),
    categories: str = typer.Option("external,erc20", "--categories", help="Transfer types"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max transfers to return"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get asset transfers for an address."""
    try:
        with AlchemyClient(chain=chain) as client:
            if not markdown:
                console.print(f"[dim]Fetching transfers on {chain}...[/]")

            cat_list = [c.strip() for c in categories.split(",")]

            if direction == "both":
                result_from = client.get_asset_transfers(
                    from_address=address, categories=cat_list, max_count=limit, chain=chain
                )
                result_to = client.get_asset_transfers(
                    to_address=address, categories=cat_list, max_count=limit, chain=chain
                )
                transfers_list = result_from.get("transfers", []) + result_to.get("transfers", [])
                transfers_list.sort(key=lambda x: x.get("blockNum", "0"), reverse=True)
                transfers_list = transfers_list[:limit]
            else:
                from_addr = address if direction == "from" else None
                to_addr = address if direction == "to" else None
                result = client.get_asset_transfers(
                    from_address=from_addr,
                    to_address=to_addr,
                    categories=cat_list,
                    max_count=limit,
                    chain=chain,
                )
                transfers_list = result.get("transfers", [])

            formatted = []
            for tx in transfers_list:
                block = int(tx.get("blockNum", "0x0"), 16) if tx.get("blockNum") else 0
                formatted.append(
                    {
                        "block": block,
                        "from": (tx.get("from", "")[:10] + "...") if tx.get("from") else "",
                        "to": (tx.get("to", "")[:10] + "...") if tx.get("to") else "",
                        "value": tx.get("value", 0),
                        "asset": tx.get("asset", "???"),
                        "category": tx.get("category", ""),
                        "hash": tx.get("hash", "")[:16] + "..." if tx.get("hash") else "",
                    }
                )

            format_output(formatted, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def tx(
    tx_hash: str = typer.Argument(..., help="Transaction hash"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get transaction details by hash."""
    try:
        with AlchemyClient(chain=chain) as client:
            if not markdown:
                console.print(f"[dim]Fetching transaction on {chain}...[/]")

            tx_data = client.get_transaction(tx_hash, chain)
            receipt = client.get_transaction_receipt(tx_hash, chain)

            if not tx_data:
                console.print("[yellow]Transaction not found.[/]")
                raise typer.Exit(1)

            if output == "json":
                print(
                    json.dumps({"transaction": tx_data, "receipt": receipt}, indent=2, default=str)
                )
            else:
                status = (
                    "Success"
                    if receipt and receipt.get("status") == "0x1"
                    else "Failed"
                    if receipt
                    else "Pending"
                )
                gas_used = int(receipt.get("gasUsed", "0x0"), 16) if receipt else 0
                gas_price = int(tx_data.get("gasPrice", "0x0"), 16)
                value_wei = int(tx_data.get("value", "0x0"), 16)
                block = (
                    int(tx_data.get("blockNumber", "0x0"), 16)
                    if tx_data.get("blockNumber")
                    else "Pending"
                )

                if markdown:
                    print(f"**Hash:** `{tx_hash}`")
                    print(f"**Status:** {status}")
                    print(f"**Block:** {block}")
                    print(f"**From:** `{tx_data.get('from', 'N/A')}`")
                    print(f"**To:** `{tx_data.get('to', 'N/A')}`")
                    print(f"**Value:** {format_wei(value_wei)} ETH")
                    print(f"**Gas Used:** {gas_used:,}")
                    print(f"**Gas Price:** {format_gwei(gas_price)}")
                else:
                    status_color = (
                        "green"
                        if status == "Success"
                        else "red"
                        if status == "Failed"
                        else "yellow"
                    )
                    console.print(f"[bold]Hash:[/] {tx_hash}")
                    console.print(f"[bold]Status:[/] [{status_color}]{status}[/]")
                    console.print(f"[bold]Block:[/] {block}")
                    console.print(f"[bold]From:[/] {tx_data.get('from', 'N/A')}")
                    console.print(f"[bold]To:[/] {tx_data.get('to', 'N/A')}")
                    console.print(f"[bold]Value:[/] {format_wei(value_wei)} ETH")
                    console.print(f"[bold]Gas Used:[/] {gas_used:,}")
                    console.print(f"[bold]Gas Price:[/] {format_gwei(gas_price)}")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("block-number")
def block_number(
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get the current block number."""
    try:
        with AlchemyClient(chain=chain) as client:
            block = client.get_block_number(chain)
            if markdown:
                print(f"**Chain:** {chain}")
                print(f"**Block:** {block:,}")
            else:
                console.print(f"[bold]{chain}:[/] Block [cyan]{block:,}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("gas-price")
def gas_price(
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain to query"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get current gas price."""
    try:
        with AlchemyClient(chain=chain) as client:
            price_wei = client.get_gas_price(chain)
            price_gwei = format_gwei(price_wei)
            if markdown:
                print(f"**Chain:** {chain}")
                print(f"**Gas Price:** {price_gwei}")
            else:
                console.print(f"[bold]{chain}:[/] Gas price [cyan]{price_gwei}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def price(
    symbols: str = typer.Argument(..., help="Comma-separated token symbols (e.g., ETH,BTC,USDT)"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get current token prices by symbol."""
    try:
        with AlchemyClient() as client:
            symbol_list = [s.strip().upper() for s in symbols.split(",")]

            if not markdown:
                console.print(f"[dim]Fetching prices for {', '.join(symbol_list)}...[/]")

            result = client.get_prices_by_symbol(symbol_list)
            data = result.get("data", [])

            formatted = []
            for item in data:
                symbol = item.get("symbol", "???")
                prices = item.get("prices", [])
                error = item.get("error")

                if error:
                    formatted.append({"symbol": symbol, "price": f"Error: {error}", "updated": ""})
                elif prices:
                    for p in prices:
                        formatted.append(
                            {
                                "symbol": symbol,
                                "price": f"${float(p.get('value', 0)):,.2f}",
                                "currency": p.get("currency", "USD"),
                                "updated": p.get("lastUpdatedAt", "")[:19],
                            }
                        )
                else:
                    formatted.append({"symbol": symbol, "price": "N/A", "updated": ""})

            format_output(formatted, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("price-address")
def price_by_address(
    address: str = typer.Argument(..., help="Token contract address"),
    chain: str = typer.Option("ethereum", "--chain", "-c", help="Chain/network"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get token price by contract address."""
    try:
        with AlchemyClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching price for {address[:10]}... on {chain}...[/]")

            network_map = {
                "ethereum": "eth-mainnet",
                "polygon": "polygon-mainnet",
                "arbitrum": "arb-mainnet",
                "optimism": "opt-mainnet",
                "base": "base-mainnet",
            }
            network = network_map.get(chain.lower(), chain)

            result = client.get_prices_by_address([{"network": network, "address": address}])
            data = result.get("data", [])

            if not data:
                console.print("[yellow]No price data found.[/]")
                raise typer.Exit(1)

            if output == "json":
                print(json.dumps(result, indent=2))
            else:
                for item in data:
                    prices = item.get("prices", [])
                    error = item.get("error")
                    if error:
                        console.print(f"[red]Error: {error}[/]")
                    elif prices:
                        for p in prices:
                            if markdown:
                                print(f"**Price:** ${float(p.get('value', 0)):,.4f}")
                                print(f"**Updated:** {p.get('lastUpdatedAt', '')[:19]}")
                            else:
                                console.print(
                                    f"[bold]Price:[/] [green]${float(p.get('value', 0)):,.4f}[/]"
                                )
                                console.print(
                                    f"[bold]Updated:[/] {p.get('lastUpdatedAt', '')[:19]}"
                                )
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
