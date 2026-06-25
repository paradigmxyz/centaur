"""CLI for Tenderly simulation, tracing, and Virtual TestNets."""

import json

import typer
from rich.console import Console
from rich.table import Table

from .client import (
    TenderlyClient,
    error_path,
    extract_call_trace,
    find_failures,
    flatten_call_trace,
    trace_skeleton,
)

app = typer.Typer(
    name="tenderly",
    help="Tenderly CLI for transaction simulation, tracing, and Virtual TestNets",
)


@app.command("health")
def health():
    """Assert tenderly connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_user()
        payload = {"ok": True, "tool": "tenderly", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "tenderly", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


vnet_app = typer.Typer(name="vnet", help="Manage and interact with Virtual TestNets")
app.add_typer(vnet_app)
console = Console()

SHOW_CHOICES = (
    "summary, trace, skeleton, events, state, assets, balances, failures, error-path, json"
)


def print_rows(rows: list[dict], output: str) -> None:
    """Print a list of dicts as JSON or a rich table."""
    if output == "json":
        print(json.dumps(rows, indent=2, default=str))
        return
    if not rows:
        console.print("[yellow]No results.[/]")
        return
    table = Table()
    for col in rows[0].keys():
        table.add_column(str(col), overflow="fold")
    for row in rows:
        table.add_row(*[str(v) for v in row.values()])
    console.print(table)


def _events(result: dict) -> list[dict]:
    """Extract decoded events from a simulate/trace response."""
    transaction = result.get("transaction") or {}
    info = transaction.get("transaction_info") or {}
    logs = info.get("logs") or result.get("logs") or []
    events = []
    for log in logs:
        raw = log.get("raw") or {}
        events.append(
            {
                "name": log.get("name") or "(unknown)",
                "contract": raw.get("address", ""),
                "inputs": ", ".join(
                    f"{i.get('soltype', {}).get('name', '?')}={i.get('value')}"
                    for i in log.get("inputs") or []
                ),
            }
        )
    return events


def _state_changes(result: dict) -> list[dict]:
    """Extract storage diffs from a simulate/trace response."""
    transaction = result.get("transaction") or {}
    info = transaction.get("transaction_info") or {}
    diffs = info.get("state_diff") or result.get("state_diff") or []
    changes = []
    for diff in diffs:
        raw_entries = diff.get("raw") or [{}]
        for raw in raw_entries:
            changes.append(
                {
                    "contract": diff.get("address") or raw.get("address", ""),
                    "variable": diff.get("soltype", {}).get("name", "")
                    if diff.get("soltype")
                    else "",
                    "key": raw.get("key", ""),
                    "before": diff.get("original", raw.get("original", "")),
                    "after": diff.get("dirty", raw.get("dirty", "")),
                }
            )
    return changes


def _asset_changes(result: dict) -> list[dict]:
    """Extract asset transfers from a simulate/trace response."""
    transaction = result.get("transaction") or {}
    info = transaction.get("transaction_info") or {}
    assets = info.get("asset_changes") or result.get("asset_changes") or []
    return [
        {
            "type": change.get("type", ""),
            "from": change.get("from", ""),
            "to": change.get("to", ""),
            "amount": change.get("amount", ""),
            "token": (change.get("token_info") or {}).get("symbol", ""),
            "usd": change.get("dollar_value", ""),
        }
        for change in assets
    ]


def _balance_changes(result: dict) -> list[dict]:
    """Extract net balance changes from a simulate/trace response."""
    transaction = result.get("transaction") or {}
    info = transaction.get("transaction_info") or {}
    diffs = info.get("balance_diff") or result.get("balance_diff") or []
    return [
        {
            "address": diff.get("address", ""),
            "before": diff.get("original", ""),
            "after": diff.get("dirty", ""),
            "is_miner": diff.get("is_miner", False),
        }
        for diff in diffs
    ]


def _print_result_view(result: dict, show: str) -> None:
    """Render the requested view of a simulate/trace response."""
    if show == "json":
        print(json.dumps(result, indent=2, default=str))
        return

    root = extract_call_trace(result)
    if show == "trace":
        print_rows(flatten_call_trace(root), "table")
    elif show == "skeleton":
        print_rows(trace_skeleton(root), "table")
    elif show == "events":
        print_rows(_events(result), "table")
    elif show == "state":
        print_rows(_state_changes(result), "table")
    elif show == "assets":
        print_rows(_asset_changes(result), "table")
    elif show == "balances":
        print_rows(_balance_changes(result), "table")
    elif show == "failures":
        print_rows(find_failures(root), "table")
    elif show == "error-path":
        print_rows(error_path(root), "table")
    else:
        transaction = result.get("transaction") or {}
        status = transaction.get("status", result.get("status"))
        status_text = "[green]Success[/]" if status else "[red]Reverted[/]"
        console.print(f"[bold]Status:[/] {status_text}")
        console.print(f"[bold]Gas used:[/] {transaction.get('gas_used', 'N/A')}")
        if transaction.get("block_number"):
            console.print(f"[bold]Block:[/] {transaction.get('block_number')}")
        error = transaction.get("error_message") or (root or {}).get("error")
        if error:
            console.print(f"[bold]Error:[/] [red]{error}[/]")
        failures = find_failures(root)
        if failures:
            console.print(f"[bold]Failed calls:[/] {len(failures)} (use --show failures)")
        simulation = result.get("simulation") or {}
        if simulation.get("id"):
            console.print(f"[bold]Simulation ID:[/] {simulation['id']}")


@app.command()
def whoami():
    """Show the authenticated Tenderly user."""
    try:
        with TenderlyClient() as client:
            user = client.get_user()
            print(json.dumps(user, indent=2, default=str))
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def projects(
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
):
    """List Tenderly projects for the configured account."""
    try:
        with TenderlyClient() as client:
            rows = [
                {
                    "name": p.get("name", ""),
                    "slug": p.get("slug", ""),
                    "id": p.get("id", ""),
                }
                for p in client.list_projects()
            ]
            print_rows(rows, output)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def networks(
    search: str = typer.Option(None, "--search", "-s", help="Filter by name or chain ID"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
):
    """List public EVM networks supported by Tenderly."""
    try:
        with TenderlyClient() as client:
            nets = client.get_networks()
            rows = [
                {
                    "id": n.get("id", ""),
                    "name": n.get("name", ""),
                    "slug": n.get("slug", ""),
                    "native_currency": (n.get("native_currency") or {}).get("symbol", "")
                    if isinstance(n.get("native_currency"), dict)
                    else n.get("native_currency", ""),
                }
                for n in nets
            ]
            if search:
                needle = search.lower()
                rows = [
                    r
                    for r in rows
                    if needle in str(r["name"]).lower()
                    or needle in str(r["slug"]).lower()
                    or needle == str(r["id"])
                ]
            print_rows(rows, output)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def contract(
    address: str = typer.Argument(..., help="Contract address (0x...)"),
    network: str = typer.Option("1", "--network", "-n", help="Network ID (e.g. 1, 10, 8453)"),
    abi: bool = typer.Option(False, "--abi", help="Print only the contract ABI"),
):
    """Get metadata (name, ABI, compiler) for a verified contract."""
    try:
        with TenderlyClient() as client:
            data = client.get_contract(network, address)
            if abi:
                contract_data = data.get("data") or data
                abi_data = contract_data.get("abi") or []
                print(json.dumps(abi_data, indent=2, default=str))
            else:
                print(json.dumps(data, indent=2, default=str))
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def simulate(
    from_address: str = typer.Option(..., "--from", "-f", help="Sender address"),
    to_address: str = typer.Option(..., "--to", "-t", help="Target contract address"),
    input_data: str = typer.Option("0x", "--input", "-i", help="Calldata (0x...)"),
    value: int = typer.Option(0, "--value", "-v", help="Native value in wei"),
    gas: int = typer.Option(8_000_000, "--gas", "-g", help="Gas limit"),
    network: str = typer.Option("1", "--network", "-n", help="Network ID"),
    block: int = typer.Option(None, "--block", "-b", help="Block number (default latest)"),
    save: bool = typer.Option(False, "--save", help="Persist the simulation in Tenderly"),
    show: str = typer.Option("summary", "--show", help=f"View: {SHOW_CHOICES}"),
):
    """Simulate a transaction on a public network without spending gas."""
    try:
        with TenderlyClient() as client:
            result = client.simulate(
                network_id=network,
                from_address=from_address,
                to_address=to_address,
                input_data=input_data,
                value=value,
                gas=gas,
                block_number=block,
                save=save,
            )
            _print_result_view(result, show)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def trace(
    tx_hash: str = typer.Argument(..., help="Transaction hash (0x...)"),
    network: str = typer.Option("1", "--network", "-n", help="Network ID"),
    show: str = typer.Option("summary", "--show", help=f"View: {SHOW_CHOICES}"),
):
    """Trace an on-chain transaction with decoded calls, events, and state diffs."""
    try:
        with TenderlyClient() as client:
            result = client.trace_transaction(network, tx_hash)
            _print_result_view(result, show)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


# --- Virtual TestNets ---


def _vnet_row(vnet: dict) -> dict:
    fork = vnet.get("fork_config") or {}
    return {
        "id": vnet.get("id", ""),
        "slug": vnet.get("slug", ""),
        "network": fork.get("network_id", ""),
        "block": fork.get("block_number", ""),
        "status": vnet.get("status", ""),
    }


@vnet_app.command("create")
def vnet_create(
    slug: str = typer.Argument(..., help="Slug for the new Virtual TestNet"),
    network: str = typer.Option("1", "--network", "-n", help="Network ID to fork"),
    name: str = typer.Option(None, "--name", help="Display name (defaults to slug)"),
    block: str = typer.Option("latest", "--block", "-b", help="Fork block number or 'latest'"),
    chain_id: int = typer.Option(None, "--chain-id", help="Custom chain ID for the vnet"),
):
    """Create a Virtual TestNet by forking a public network."""
    try:
        with TenderlyClient() as client:
            block_number = int(block) if block != "latest" else "latest"
            vnet = client.create_vnet(
                network_id=network,
                slug=slug,
                display_name=name,
                block_number=block_number,
                chain_id=chain_id,
            )
            print(json.dumps(vnet, indent=2, default=str))
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("list")
def vnet_list(
    page: int = typer.Option(1, "--page", help="Page number"),
    per_page: int = typer.Option(20, "--per-page", help="Results per page"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
):
    """List Virtual TestNets in the project."""
    try:
        with TenderlyClient() as client:
            vnets = client.list_vnets(page=page, per_page=per_page)
            if output == "json":
                print(json.dumps(vnets, indent=2, default=str))
            else:
                print_rows([_vnet_row(v) for v in vnets], output)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("get")
def vnet_get(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
):
    """Get full details of a Virtual TestNet, including RPC URLs."""
    try:
        with TenderlyClient() as client:
            print(json.dumps(client.get_vnet(vnet_id), indent=2, default=str))
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("delete")
def vnet_delete(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
):
    """Delete a Virtual TestNet."""
    try:
        with TenderlyClient() as client:
            client.delete_vnet(vnet_id)
            console.print(f"[green]Deleted vnet {vnet_id}.[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("txs")
def vnet_txs(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    page: int = typer.Option(1, "--page", help="Page number"),
    per_page: int = typer.Option(20, "--per-page", help="Results per page"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
):
    """List transactions executed on a Virtual TestNet."""
    try:
        with TenderlyClient() as client:
            txs = client.list_vnet_transactions(vnet_id, page=page, per_page=per_page)
            if output == "json":
                print(json.dumps(txs, indent=2, default=str))
            else:
                rows = [
                    {
                        "hash": tx.get("tx_hash") or tx.get("hash", ""),
                        "from": tx.get("from", ""),
                        "to": tx.get("to", ""),
                        "status": tx.get("status", ""),
                        "block": tx.get("block_number", ""),
                    }
                    for tx in txs
                ]
                print_rows(rows, output)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("send")
def vnet_send(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    from_address: str = typer.Option(
        ..., "--from", "-f", help="Sender (impersonated, no key needed)"
    ),
    to_address: str = typer.Option(..., "--to", "-t", help="Target address"),
    input_data: str = typer.Option("0x", "--input", "-i", help="Calldata (0x...)"),
    value: int = typer.Option(0, "--value", "-v", help="Native value in wei"),
    gas: int = typer.Option(None, "--gas", "-g", help="Gas limit"),
):
    """Send an unsigned (impersonated) transaction on a Virtual TestNet."""
    try:
        with TenderlyClient() as client:
            tx_hash = client.send_vnet_transaction(
                vnet_id,
                from_address=from_address,
                to_address=to_address,
                input_data=input_data,
                value=value,
                gas=gas,
            )
            console.print(f"[green]Sent:[/] {tx_hash}")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("simulate")
def vnet_simulate(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    from_address: str = typer.Option(..., "--from", "-f", help="Sender address"),
    to_address: str = typer.Option(..., "--to", "-t", help="Target address"),
    input_data: str = typer.Option("0x", "--input", "-i", help="Calldata (0x...)"),
    value: int = typer.Option(0, "--value", "-v", help="Native value in wei"),
    gas: int = typer.Option(8_000_000, "--gas", "-g", help="Gas limit"),
    show: str = typer.Option("summary", "--show", help=f"View: {SHOW_CHOICES}"),
):
    """Simulate a transaction on a Virtual TestNet without changing its state."""
    try:
        with TenderlyClient() as client:
            result = client.simulate_vnet_transaction(
                vnet_id,
                from_address=from_address,
                to_address=to_address,
                input_data=input_data,
                value=value,
                gas=gas,
            )
            _print_result_view(result, show)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("call")
def vnet_call(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    to_address: str = typer.Option(..., "--to", "-t", help="Contract address"),
    input_data: str = typer.Option(..., "--input", "-i", help="Calldata (0x...)"),
    from_address: str = typer.Option(None, "--from", "-f", help="Caller address"),
):
    """Execute a read-only eth_call on a Virtual TestNet."""
    try:
        with TenderlyClient() as client:
            result = client.vnet_call(vnet_id, to_address, input_data, from_address=from_address)
            print(result)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("fund")
def vnet_fund(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    addresses: str = typer.Argument(..., help="Comma-separated addresses to fund"),
    amount: float = typer.Option(10.0, "--amount", "-a", help="Amount in native token (e.g. ETH)"),
):
    """Set the native balance of one or more accounts."""
    try:
        with TenderlyClient() as client:
            addr_list = [a.strip() for a in addresses.split(",")]
            wei = int(amount * 10**18)
            client.set_balance(vnet_id, addr_list, wei)
            console.print(f"[green]Set balance of {len(addr_list)} account(s) to {amount}.[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("erc20")
def vnet_erc20(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    token: str = typer.Option(..., "--token", help="ERC-20 token contract address"),
    wallet: str = typer.Option(..., "--wallet", "-w", help="Wallet address"),
    amount: int = typer.Option(..., "--amount", "-a", help="Amount in token base units"),
):
    """Set the ERC-20 token balance of a wallet."""
    try:
        with TenderlyClient() as client:
            client.set_erc20_balance(vnet_id, token, wallet, amount)
            console.print(f"[green]Set {token} balance of {wallet} to {amount}.[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("storage")
def vnet_storage(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    contract: str = typer.Option(..., "--contract", "-c", help="Contract address"),
    slot: str = typer.Option(..., "--slot", "-s", help="Storage slot (0x...)"),
    value: str = typer.Option(..., "--value", "-v", help="32-byte value (0x...)"),
):
    """Write a raw value to a contract storage slot."""
    try:
        with TenderlyClient() as client:
            client.set_storage_at(vnet_id, contract, slot, value)
            console.print(f"[green]Wrote slot {slot} on {contract}.[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("snapshot")
def vnet_snapshot(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
):
    """Save the current Virtual TestNet state and print the snapshot ID."""
    try:
        with TenderlyClient() as client:
            snapshot_id = client.snapshot(vnet_id)
            console.print(f"[green]Snapshot:[/] {snapshot_id}")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("revert")
def vnet_revert(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    snapshot_id: str = typer.Argument(..., help="Snapshot ID from 'tenderly vnet snapshot'"),
):
    """Restore a Virtual TestNet to a previously saved snapshot."""
    try:
        with TenderlyClient() as client:
            client.revert(vnet_id, snapshot_id)
            console.print(f"[green]Reverted to snapshot {snapshot_id}.[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("increase-time")
def vnet_increase_time(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    seconds: int = typer.Argument(..., help="Seconds to advance the clock"),
):
    """Advance the Virtual TestNet clock."""
    try:
        with TenderlyClient() as client:
            client.increase_time(vnet_id, seconds)
            console.print(f"[green]Advanced time by {seconds}s.[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@vnet_app.command("mine")
def vnet_mine(
    vnet_id: str = typer.Argument(..., help="Virtual TestNet ID"),
    blocks: int = typer.Option(1, "--blocks", "-b", help="Number of blocks to mine"),
):
    """Mine one or more blocks on a Virtual TestNet."""
    try:
        with TenderlyClient() as client:
            client.mine_blocks(vnet_id, blocks)
            console.print(f"[green]Mined {blocks} block(s).[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
