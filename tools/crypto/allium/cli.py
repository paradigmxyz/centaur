"""CLI for Allium on-chain analytics."""

from dotenv import load_dotenv

load_dotenv()

import json
import sys

import typer
from rich.console import Console

from centaur_sdk import Table

from .client import AlliumClient, get_example_queries

app = typer.Typer(name="allium", help="Allium CLI for on-chain stablecoin and DeFi analytics")


@app.command("health")
def health():
    """Assert allium connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search_schemas("ethereum")
        payload = {"ok": True, "tool": "allium", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "allium", "error": str(exc), "details": {}}
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


def parse_params(params_str: str | None) -> dict | None:
    """Parse key=value,key=value format into dict."""
    if not params_str:
        return None
    result = {}
    for pair in params_str.split(","):
        if "=" not in pair:
            continue
        key, value = pair.split("=", 1)
        try:
            result[key.strip()] = int(value.strip())
        except ValueError:
            result[key.strip()] = value.strip()
    return result


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
def sql(
    query_sql: str = typer.Argument(..., help="SQL query to execute"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max rows to return"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Execute arbitrary SQL directly against Allium.

    This uses the MCP endpoint to run SQL without needing a saved query.

    Examples:
        allium sql "SELECT COUNT(*) FROM ethereum.raw.transactions"
        allium sql "SELECT * FROM polygon.predictions.trades LIMIT 10"
        allium sql "SELECT MAX(block_timestamp) FROM polygon.predictions.trades" -o json
    """
    try:
        with AlliumClient() as client:
            if not markdown:
                console.print("[dim]Executing SQL...[/]")
            results = client.run_sql(query_sql, row_limit=limit)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("search-schemas")
def search_schemas(
    search_query: str = typer.Argument(..., help="Semantic search query"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Search Allium schemas using semantic search.

    Examples:
        allium search-schemas "prediction markets"
        allium search-schemas "erc20 token transfers"
        allium search-schemas "kalshi trades"
    """
    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Searching schemas for: {search_query}...[/]")
            tables = client.search_schemas(search_query)
            if markdown:
                for t in tables:
                    print(f"- `{t}`")
            else:
                console.print("[bold]Matching tables:[/]")
                for t in tables:
                    console.print(f"  [cyan]{t}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("describe")
def describe_table(
    table_id: str = typer.Argument(..., help="Full table name (e.g., polygon.predictions.trades)"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Fetch schema metadata for a table.

    Examples:
        allium describe polygon.predictions.trades
        allium describe ethereum.raw.token_transfers -o json
    """
    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching schema for {table_id}...[/]")
            schema = client.fetch_schema(table_id)
            if output == "json":
                print(json.dumps(schema, indent=2, default=str))
            elif markdown:
                print(f"## {table_id}")
                if "description" in schema:
                    print(f"\n{schema['description']}\n")
                if "columns" in schema:
                    print("\n| Column | Type | Description |")
                    print("| --- | --- | --- |")
                    for col in schema["columns"]:
                        print(
                            f"| `{col.get('name', '')}` | {col.get('type', '')} | {col.get('description', '')} |"
                        )
            else:
                console.print(f"[bold]{table_id}[/]")
                if "description" in schema:
                    console.print(f"[dim]{schema['description']}[/]\n")
                if "columns" in schema:
                    table = Table()
                    table.add_column("Column", style="cyan")
                    table.add_column("Type", style="yellow")
                    table.add_column("Description", style="white")
                    for col in schema["columns"]:
                        table.add_row(
                            col.get("name", ""),
                            col.get("type", ""),
                            col.get("description", ""),
                        )
                    console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


PREDICTION_TABLES = {
    "polygon.predictions.trades": "block_timestamp",
    "polygon.predictions.markets": "end_date",
    "common.predictions.kalshi_trades": "created_time",
}

HYPERLIQUID_TABLES = {
    "hyperliquid.dex.trades": "timestamp",
    "hyperliquid.raw.trades": "timestamp",
    "hyperliquid.raw.fills": "timestamp",
    "hyperliquid.raw.orders": "status_change_timestamp",
    "hyperliquid.raw.blocks": "timestamp",
    "hyperliquid.raw.transactions": "block_timestamp",
    "hyperliquid.metrics.overview": "day",
    "hyperliquid.assets.transfers": "block_timestamp",
    "hyperliquid.raw.perpetual_market_asset_contexts": "timestamp",
}


@app.command("check-freshness")
def check_freshness(
    tables: str = typer.Argument(
        None,
        help="Comma-separated table names, or 'predictions'/'hyperliquid' for all tables in that category",
    ),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Check when tables were last updated.

    Uses ORDER BY ... LIMIT 1 instead of MAX() for faster results on large tables.

    Examples:
        allium check-freshness polygon.predictions.trades
        allium check-freshness predictions
        allium check-freshness hyperliquid
        allium check-freshness "hyperliquid.dex.trades,hyperliquid.raw.fills"
    """
    if tables == "predictions" or tables is None:
        tables_to_check = list(PREDICTION_TABLES.keys())
    elif tables == "hyperliquid":
        tables_to_check = list(HYPERLIQUID_TABLES.keys())
    else:
        tables_to_check = [t.strip() for t in tables.split(",")]

    try:
        results = []
        with AlliumClient() as client:
            for i, table_name in enumerate(tables_to_check):
                if not markdown:
                    console.print(
                        f"[dim]({i + 1}/{len(tables_to_check)}) Checking {table_name}...[/]"
                    )
                try:
                    timestamp_col = (
                        PREDICTION_TABLES.get(table_name)
                        or HYPERLIQUID_TABLES.get(table_name)
                        or "block_timestamp"
                    )
                    sql = f"SELECT {timestamp_col} as last_update FROM {table_name} ORDER BY {timestamp_col} DESC LIMIT 1"
                    result = client.run_sql(sql, row_limit=1)
                    last_update = result[0].get("last_update") if result else "N/A"
                    results.append({"table": table_name, "last_update": str(last_update)})
                except Exception as e:
                    results.append({"table": table_name, "last_update": f"ERROR: {e}"})

            if markdown:
                print("| Table | Last Update |")
                print("| --- | --- |")
                for r in results:
                    status = (
                        "✅"
                        if "ERROR" not in r["last_update"] and "N/A" not in r["last_update"]
                        else "❌"
                    )
                    print(f"| `{r['table']}` | {status} {r['last_update']} |")
            else:
                table = Table()
                table.add_column("Table", style="cyan")
                table.add_column("Last Update", style="white")
                for r in results:
                    table.add_row(r["table"], r["last_update"])
                console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def query(
    query_id: str = typer.Argument(..., help="Saved query ID from Allium Explorer"),
    params: str = typer.Option(None, "--params", "-p", help="Parameters as key=value,key=value"),
    wait: bool = typer.Option(True, "--wait/--no-wait", help="Wait for results"),
    timeout: int = typer.Option(300, "--timeout", "-t", help="Max seconds to wait"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    limit: int = typer.Option(10000, "--limit", "-n", help="Max rows to return"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Run a saved Allium query.

    Examples:
        allium query abc123
        allium query abc123 --params "days=30,chain=ethereum"
        allium query abc123 --no-wait
        allium query abc123 -o json
        allium query abc123 -m
    """
    parameters = parse_params(params)

    try:
        with AlliumClient() as client:
            if wait:
                if not markdown:
                    console.print(f"[dim]Running query {query_id}...[/]")
                results = client.execute_query(query_id, parameters=parameters, timeout=timeout)
                format_output(results[:limit], output, markdown)
            else:
                run_id = client.run_query(query_id, parameters=parameters, row_limit=limit)
                if markdown:
                    print(f"Query started. Run ID: {run_id}")
                    print(f"Use `allium status {run_id}` to check status")
                else:
                    console.print("[green]Query started[/]")
                    console.print(f"Run ID: [cyan]{run_id}[/]")
                    console.print(f"[dim]Use 'allium status {run_id}' to check status[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def status(
    run_id: str = typer.Argument(..., help="Query run ID"),
):
    """Check status of a query run.

    Examples:
        allium status abc123-run-456
    """
    try:
        with AlliumClient() as client:
            result = client.get_query_status(run_id)
            state = result.get("status", "unknown")

            if state == "success":
                console.print("[green]✓ Query completed successfully[/]")
            elif state == "failed":
                console.print("[red]✗ Query failed[/]")
                if "error" in result:
                    console.print(f"[red]{result['error']}[/]")
            elif state in ("pending", "running"):
                console.print(f"[yellow]⏳ Query is {state}...[/]")
            else:
                console.print(f"[dim]Status: {state}[/]")

            console.print(f"[dim]{json.dumps(result, indent=2)}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def results(
    run_id: str = typer.Argument(..., help="Query run ID"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max rows to display"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get results of a completed query run.

    Examples:
        allium results abc123-run-456
        allium results abc123-run-456 -o json
        allium results abc123-run-456 -m
    """
    try:
        with AlliumClient() as client:
            data = client.get_query_results(run_id)
            format_output(data[:limit], output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def stablecoin_volume(
    chain: str = typer.Option(
        None, "--chain", "-c", help="Filter by chain (ethereum, polygon, etc.)"
    ),
    stablecoin: str = typer.Option(
        None, "--stablecoin", "-s", help="Filter by stablecoin (USDC, USDT, etc.)"
    ),
    days: int = typer.Option(30, "--days", "-d", help="Number of days to analyze"),
    query_id: str = typer.Option(
        None, "--query-id", "-q", help="Saved query ID for volume analysis"
    ),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get stablecoin transfer volumes.

    Requires a saved query in Allium Explorer. Use sql-examples to get the query template.

    Examples:
        allium stablecoin-volume --query-id abc123
        allium stablecoin-volume -c ethereum -d 7 -q abc123
        allium stablecoin-volume -q abc123 -m
    """
    if not query_id:
        if markdown:
            print("This command requires a saved query ID.")
            print("1. Create a query in Allium Explorer using `allium sql-examples`")
            print("2. Save the query and get its ID")
            print("3. Run: `allium stablecoin-volume --query-id YOUR_QUERY_ID`")
        else:
            console.print("[yellow]This command requires a saved query ID.[/]")
            console.print(
                "[dim]1. Create a query in Allium Explorer using 'allium sql-examples'[/]"
            )
            console.print("[dim]2. Save the query and get its ID[/]")
            console.print("[dim]3. Run: allium stablecoin-volume --query-id YOUR_QUERY_ID[/]")
        raise typer.Exit(1)

    parameters = {"days": days}
    if chain:
        parameters["chain"] = chain
    if stablecoin:
        parameters["stablecoin"] = stablecoin

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching stablecoin volume data for last {days} days...[/]")
            results = client.execute_query(query_id, parameters=parameters)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def top_contracts(
    chain: str = typer.Argument(..., help="Chain to analyze (ethereum, polygon, etc.)"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to analyze"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max contracts to return"),
    query_id: str = typer.Option(None, "--query-id", "-q", help="Saved query ID for top contracts"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Find top contracts by stablecoin volume.

    Requires a saved query in Allium Explorer. Use sql-examples to get the query template.

    Examples:
        allium top-contracts ethereum --query-id abc123
        allium top-contracts polygon -d 30 -n 50 -q abc123
        allium top-contracts ethereum -q abc123 -m
    """
    if not query_id:
        if markdown:
            print("This command requires a saved query ID.")
            print("1. Create a query in Allium Explorer using `allium sql-examples`")
            print("2. Save the query and get its ID")
            print("3. Run: `allium top-contracts ethereum --query-id YOUR_QUERY_ID`")
        else:
            console.print("[yellow]This command requires a saved query ID.[/]")
            console.print(
                "[dim]1. Create a query in Allium Explorer using 'allium sql-examples'[/]"
            )
            console.print("[dim]2. Save the query and get its ID[/]")
            console.print("[dim]3. Run: allium top-contracts ethereum --query-id YOUR_QUERY_ID[/]")
        raise typer.Exit(1)

    parameters = {"chain": chain, "days": days, "limit": limit}

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Finding top contracts on {chain} for last {days} days...[/]")
            results = client.execute_query(query_id, parameters=parameters)
            format_output(results[:limit], output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def raw(
    method: str = typer.Argument(..., help="HTTP method (GET, POST, PUT, DELETE)"),
    endpoint: str = typer.Argument(..., help="API endpoint path"),
    data: str = typer.Option(None, "--data", "-d", help="JSON body for POST/PUT requests"),
):
    """Make a raw API call.

    Examples:
        allium raw GET /api/v1/explorer/queries
        allium raw POST /api/v1/explorer/queries/abc123/run-async -d '{"row_limit": 100}'
    """
    json_data = None
    if data:
        try:
            json_data = json.loads(data)
        except json.JSONDecodeError as e:
            console.print(f"[red]Invalid JSON: {e}[/]")
            raise typer.Exit(1)

    try:
        with AlliumClient() as client:
            result = client.raw_request(method.upper(), endpoint, json_data=json_data)
            print(json.dumps(result, indent=2, default=str), file=sys.stdout)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("sql-examples")
def sql_examples(
    query_name: str = typer.Argument(
        None, help="Specific query to show (volume_by_chain, top_contracts, etc.)"
    ),
):
    """Print example SQL queries for common stablecoin analysis.

    These queries can be saved in Allium Explorer and used with other commands.

    Available queries:
    - volume_by_chain: Stablecoin volume aggregated by chain
    - top_contracts: Top contracts by stablecoin transfer volume
    - stablecoin_flows: Net inflows/outflows by address
    - transfers: Recent stablecoin transfers
    - daily_metrics: Pre-aggregated daily metrics
    - dex_trades: DEX trades involving stablecoins
    - cex_identification: Identify potential CEX wallets

    Examples:
        allium sql-examples
        allium sql-examples volume_by_chain
    """
    queries = get_example_queries()

    if query_name:
        if query_name not in queries:
            console.print(f"[red]Unknown query: {query_name}[/]")
            console.print(f"[dim]Available: {', '.join(queries.keys())}[/]")
            raise typer.Exit(1)

        console.print(f"\n[bold cyan]{query_name}[/]")
        console.print("=" * 60)
        console.print(queries[query_name])
    else:
        console.print("\n[bold]Example SQL Queries for Stablecoin Analysis[/]")
        console.print("=" * 60)
        console.print(
            "[dim]Save these in Allium Explorer, then use the query ID with CLI commands.[/]\n"
        )

        for name, sql in queries.items():
            console.print(f"\n[bold cyan]{name}[/]")
            console.print("-" * 40)
            console.print(sql)


@app.command()
def tables(
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List key Allium tables for stablecoin analysis."""
    tables_info = [
        ("crosschain.stablecoin.list", "Stablecoin contract addresses by chain"),
        ("crosschain.stablecoin.transfers", "All stablecoin transfers (USDC, USDT, DAI, etc.)"),
        ("crosschain.metrics.stablecoin_volume", "Pre-aggregated daily stablecoin metrics"),
        ("crosschain.dex.trades", "DEX trades across chains"),
        ("crosschain.dex.pools", "DEX pool information"),
        ("ethereum.core.transactions", "Ethereum raw transactions"),
        ("ethereum.core.traces", "Ethereum internal transactions"),
    ]

    if markdown:
        print_markdown_table(["Table", "Description"], [[name, desc] for name, desc in tables_info])
        print("\nExplore schema at https://app.allium.so")
        return

    console.print("\n[bold]Key Allium Tables for Stablecoin Analysis[/]")
    console.print("=" * 60)

    table = Table()
    table.add_column("Table", style="cyan")
    table.add_column("Description", style="white")

    for name, desc in tables_info:
        table.add_row(name, desc)

    console.print(table)
    console.print("\n[dim]Explore schema at https://app.allium.so[/]")


# ============================================================================
# Hyperliquid Commands
# ============================================================================


@app.command("hl-trades")
def hyperliquid_trades(
    coin: str = typer.Option(
        None, "--coin", "-c", help="Filter by coin symbol (e.g., HYPE, BTC, ETH)"
    ),
    address: str = typer.Option(None, "--address", "-a", help="Filter by user address"),
    side: str = typer.Option(
        None, "--side", "-s", help="Filter by side: B (buy/long) or A (sell/short)"
    ),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to look back"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max rows to return"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Query Hyperliquid DEX trades.

    Examples:
        allium hl-trades
        allium hl-trades --coin HYPE -d 1
        allium hl-trades --address 0x1234... -n 50
        allium hl-trades --side B --coin BTC
    """
    where_clauses = [f"timestamp >= CURRENT_TIMESTAMP - INTERVAL '{days} days'"]
    if coin:
        where_clauses.append(f"coin = '{coin}'")
    if address:
        where_clauses.append(f"(buyer = '{address}' OR seller = '{address}')")
    if side:
        where_clauses.append(f"side = '{side.upper()}'")

    where_sql = " AND ".join(where_clauses)
    sql = f"""
    SELECT
        timestamp,
        coin,
        side,
        price,
        size,
        price * size as notional_usd,
        buyer,
        seller,
        trade_id
    FROM hyperliquid.dex.trades
    WHERE {where_sql}
    ORDER BY timestamp DESC
    LIMIT {limit}
    """

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching Hyperliquid trades (last {days} days)...[/]")
            results = client.run_sql(sql, row_limit=limit)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("hl-volume")
def hyperliquid_volume(
    coin: str = typer.Option(None, "--coin", "-c", help="Filter by coin symbol"),
    days: int = typer.Option(30, "--days", "-d", help="Number of days to analyze"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Hyperliquid trading volume by coin.

    Examples:
        allium hl-volume
        allium hl-volume --coin HYPE -d 7
        allium hl-volume -d 30 -o json
    """
    coin_filter = f"AND coin = '{coin}'" if coin else ""
    sql = f"""
    SELECT
        coin,
        COUNT(*) as trade_count,
        SUM(price * size) as total_volume_usd,
        AVG(price) as avg_price,
        MIN(price) as min_price,
        MAX(price) as max_price,
        COUNT(DISTINCT buyer) as unique_buyers,
        COUNT(DISTINCT seller) as unique_sellers
    FROM hyperliquid.dex.trades
    WHERE timestamp >= CURRENT_TIMESTAMP - INTERVAL '{days} days'
    {coin_filter}
    GROUP BY coin
    ORDER BY total_volume_usd DESC
    """

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching Hyperliquid volume (last {days} days)...[/]")
            results = client.run_sql(sql)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("hl-orders")
def hyperliquid_orders(
    coin: str = typer.Option(None, "--coin", "-c", help="Filter by coin symbol"),
    address: str = typer.Option(None, "--address", "-a", help="Filter by user address"),
    status: str = typer.Option(None, "--status", help="Filter by status (filled, canceled, etc.)"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to look back"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max rows to return"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Query Hyperliquid orders.

    Examples:
        allium hl-orders --coin HYPE
        allium hl-orders --address 0x1234... -d 1
        allium hl-orders --status filled
    """
    where_clauses = [f"status_change_timestamp >= CURRENT_TIMESTAMP - INTERVAL '{days} days'"]
    if coin:
        where_clauses.append(f"coin = '{coin}'")
    if address:
        where_clauses.append(f"\"user\" = '{address}'")
    if status:
        where_clauses.append(f"status = '{status}'")

    where_sql = " AND ".join(where_clauses)
    sql = f"""
    SELECT
        status_change_timestamp,
        coin,
        side,
        price,
        original_size,
        status,
        order_id,
        "user"
    FROM hyperliquid.raw.orders
    WHERE {where_sql}
    ORDER BY status_change_timestamp DESC
    LIMIT {limit}
    """

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching Hyperliquid orders (last {days} days)...[/]")
            results = client.run_sql(sql, row_limit=limit)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("hl-metrics")
def hyperliquid_metrics(
    days: int = typer.Option(30, "--days", "-d", help="Number of days to analyze"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Hyperliquid daily overview metrics.

    Returns daily metrics including transactions, active users, TVL, and bridge flows.

    Examples:
        allium hl-metrics
        allium hl-metrics -d 7
        allium hl-metrics -o json
    """
    sql = f"""
    SELECT *
    FROM hyperliquid.metrics.overview
    WHERE day >= CURRENT_DATE - INTERVAL '{days} days'
    ORDER BY day DESC
    """

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching Hyperliquid metrics (last {days} days)...[/]")
            results = client.run_sql(sql)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("hl-funding")
def hyperliquid_funding(
    coin: str = typer.Option(None, "--coin", "-c", help="Filter by coin symbol"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to analyze"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Hyperliquid perpetual funding rates and open interest.

    Examples:
        allium hl-funding
        allium hl-funding --coin HYPE -d 1
        allium hl-funding -o json
    """
    coin_filter = f"AND coin = '{coin}'" if coin else ""
    sql = f"""
    SELECT
        timestamp,
        coin,
        funding,
        open_interest,
        premium,
        mark_price,
        mid_price
    FROM hyperliquid.raw.perpetual_market_asset_contexts
    WHERE timestamp >= CURRENT_TIMESTAMP - INTERVAL '{days} days'
    {coin_filter}
    ORDER BY timestamp DESC
    LIMIT 1000
    """

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching Hyperliquid funding data (last {days} days)...[/]")
            results = client.run_sql(sql)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("hl-builders")
def hyperliquid_builders(
    builder: str = typer.Option(None, "--builder", "-b", help="Filter by builder address"),
    days: int = typer.Option(7, "--days", "-d", help="Number of days to analyze"),
    output: str = typer.Option("table", "--output", "-o", help="Output format: table, json, csv"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get Hyperliquid builder fees and activity.

    Examples:
        allium hl-builders
        allium hl-builders -d 30
        allium hl-builders --builder 0x1234...
    """
    builder_filter = f"AND builder_address = '{builder}'" if builder else ""
    sql = f"""
    SELECT
        builder_address,
        COALESCE(l.builder_name, 'Unknown') as builder_name,
        COUNT(*) as fill_count,
        SUM(builder_fee) as total_fees,
        COUNT(DISTINCT "user") as unique_users
    FROM hyperliquid.raw.builder_fills f
    LEFT JOIN hyperliquid.raw.builder_labels l ON f.builder_address = l.address
    WHERE timestamp >= CURRENT_TIMESTAMP - INTERVAL '{days} days'
    {builder_filter}
    GROUP BY builder_address, l.builder_name
    ORDER BY total_fees DESC
    """

    try:
        with AlliumClient() as client:
            if not markdown:
                console.print(f"[dim]Fetching Hyperliquid builder data (last {days} days)...[/]")
            results = client.run_sql(sql)
            format_output(results, output, markdown)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command("hl-tables")
def hyperliquid_tables(
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List available Hyperliquid tables in Allium."""
    tables_info = [
        ("hyperliquid.dex.trades", "Enriched DEX trades with token metadata and builder fees"),
        ("hyperliquid.raw.trades", "Raw trade data (two fills make a trade)"),
        ("hyperliquid.raw.fills", "Raw fills data for all executed trades"),
        ("hyperliquid.raw.orders", "Order book data with status changes"),
        ("hyperliquid.raw.blocks", "Block-level metadata (height, hash, proposer)"),
        ("hyperliquid.raw.transactions", "Transaction data for each block"),
        ("hyperliquid.raw.tokens", "Token metadata (spot tokens only)"),
        ("hyperliquid.raw.perpetual_market_asset_contexts", "Funding rates, OI, premium, prices"),
        ("hyperliquid.raw.builder_fills", "Builder-facilitated fills with fees"),
        ("hyperliquid.raw.builder_transactions", "Transactions with builder info"),
        ("hyperliquid.raw.builder_labels", "Builder address to name mapping"),
        ("hyperliquid.metrics.overview", "Daily overview metrics (txns, users, TVL)"),
        ("hyperliquid.assets.transfers", "Token transfers (deposits/withdrawals)"),
    ]

    if markdown:
        print_markdown_table(["Table", "Description"], [[name, desc] for name, desc in tables_info])
        print("\nDocs: https://docs.allium.so/historical-data/supported-blockchains/hyperliquid")
        return

    console.print("\n[bold]Hyperliquid Tables in Allium[/]")
    console.print("=" * 70)

    table = Table()
    table.add_column("Table", style="cyan")
    table.add_column("Description", style="white")

    for name, desc in tables_info:
        table.add_row(name, desc)

    console.print(table)
    console.print(
        "\n[dim]Docs: https://docs.allium.so/historical-data/supported-blockchains/hyperliquid[/]"
    )


if __name__ == "__main__":
    app()
