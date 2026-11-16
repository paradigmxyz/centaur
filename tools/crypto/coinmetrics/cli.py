"""CLI for Coin Metrics API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="coinmetrics", help="Coin Metrics CLI for crypto market and on-chain data")


@app.command("health")
def health():
    """Assert coinmetrics connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_assets()
        payload = {"ok": True, "tool": "coinmetrics", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "coinmetrics", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import CoinMetricsClient

    return CoinMetricsClient()


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def format_number(value: float, decimals: int = 2) -> str:
    """Format large numbers with B/M/K suffixes."""
    if value >= 1e12:
        return f"{value / 1e12:.{decimals}f}T"
    elif value >= 1e9:
        return f"{value / 1e9:.{decimals}f}B"
    elif value >= 1e6:
        return f"{value / 1e6:.{decimals}f}M"
    elif value >= 1e3:
        return f"{value / 1e3:.{decimals}f}K"
    return f"{value:.{decimals}f}"


@app.command()
def assets(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List available assets."""
    client = get_client()
    data = client.list_assets()[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = [
            [a.get("asset", ""), a.get("full_name", ""), a.get("asset_class", "")] for a in data
        ]
        print_markdown_table(["Asset", "Name", "Class"], rows)
        return

    table = Table(title="Available Assets")
    table.add_column("Asset", style="cyan")
    table.add_column("Name", style="green", max_width=30)
    table.add_column("Class", style="yellow")

    for a in data:
        table.add_row(a.get("asset", ""), a.get("full_name", ""), a.get("asset_class", ""))

    console.print(table)


@app.command()
def metrics(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results to show"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List available metrics."""
    client = get_client()
    data = client.list_metrics()[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = [
            [
                m.get("metric", ""),
                m.get("full_name", "")[:50],
                m.get("category", ""),
            ]
            for m in data
        ]
        print_markdown_table(["Metric", "Name", "Category"], rows)
        return

    table = Table(title="Available Metrics")
    table.add_column("Metric", style="cyan")
    table.add_column("Name", style="green", max_width=40)
    table.add_column("Category", style="yellow")

    for m in data:
        table.add_row(
            m.get("metric", ""),
            m.get("full_name", "")[:40],
            m.get("category", ""),
        )

    console.print(table)


@app.command()
def timeseries(
    assets_arg: str = typer.Argument(..., help="Comma-separated assets (e.g., btc,eth)"),
    metrics_arg: str = typer.Argument(..., help="Comma-separated metrics (e.g., PriceUSD)"),
    start: str = typer.Option(None, "--start", "-s", help="Start time (ISO8601)"),
    end: str = typer.Option(None, "--end", "-e", help="End time (ISO8601)"),
    frequency: str = typer.Option("1d", "--frequency", "-f", help="Frequency (1b,1s,1m,5m,1h,1d)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get metric timeseries for assets."""
    client = get_client()
    data = client.get_asset_metrics(
        assets=assets_arg,
        metrics=metrics_arg,
        frequency=frequency,
        start_time=start,
        end_time=end,
        page_size=limit,
    )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No data returned[/]")
        return

    metric_keys = [k for k in data[0].keys() if k not in ("asset", "time")]

    if markdown:
        headers = ["Asset", "Time"] + metric_keys
        rows = [
            [d.get("asset", ""), d.get("time", "")] + [str(d.get(k, "")) for k in metric_keys]
            for d in data
        ]
        print_markdown_table(headers, rows)
        return

    table = Table(title=f"Timeseries: {assets_arg} - {metrics_arg}")
    table.add_column("Asset", style="cyan")
    table.add_column("Time", style="dim")
    for k in metric_keys:
        table.add_column(k, style="yellow", justify="right")

    for d in data:
        row = [d.get("asset", ""), d.get("time", "")[:19]]
        for k in metric_keys:
            val = d.get(k, "")
            if isinstance(val, (int, float)):
                row.append(format_number(val))
            else:
                row.append(str(val) if val else "")
        table.add_row(*row)

    console.print(table)


@app.command("asset-metrics")
def asset_metrics(
    asset: str = typer.Argument(..., help="Asset symbol (e.g., btc)"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get available metrics for an asset."""
    client = get_client()
    data = client.get_catalog_asset_metrics(assets=asset)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print(f"[yellow]No metrics found for {asset}[/]")
        return

    asset_data = data[0] if data else {}
    metrics_list = asset_data.get("metrics", [])[:limit]

    if markdown:
        rows = [
            [
                m.get("metric", ""),
                ", ".join(m.get("frequencies", [])[:3]),
                m.get("min_time", "")[:10] if m.get("min_time") else "",
            ]
            for m in metrics_list
        ]
        print_markdown_table(["Metric", "Frequencies", "Since"], rows)
        return

    table = Table(title=f"Available Metrics for {asset.upper()}")
    table.add_column("Metric", style="cyan")
    table.add_column("Frequencies", style="yellow")
    table.add_column("Since", style="dim")

    for m in metrics_list:
        freqs = ", ".join(m.get("frequencies", [])[:3])
        since = m.get("min_time", "")[:10] if m.get("min_time") else ""
        table.add_row(m.get("metric", ""), freqs, since)

    console.print(table)


@app.command("market-data")
def market_data(
    markets: str = typer.Argument(
        ..., help="Comma-separated markets (e.g., coinbase-btc-usd-spot)"
    ),
    data_type: str = typer.Option("candles", "--type", "-t", help="Data type: candles or trades"),
    start: str = typer.Option(None, "--start", "-s", help="Start time (ISO8601)"),
    end: str = typer.Option(None, "--end", "-e", help="End time (ISO8601)"),
    frequency: str = typer.Option("1h", "--frequency", "-f", help="Candle frequency (1m,5m,1h,1d)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Get market candles or trades."""
    client = get_client()

    if data_type == "trades":
        data = client.get_market_trades(
            markets=markets,
            start_time=start,
            end_time=end,
            page_size=limit,
        )
    else:
        data = client.get_market_candles(
            markets=markets,
            frequency=frequency,
            start_time=start,
            end_time=end,
            page_size=limit,
        )

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if not data:
        console.print("[yellow]No data returned[/]")
        return

    if data_type == "trades":
        if markdown:
            rows = [
                [
                    d.get("market", ""),
                    d.get("time", "")[:19],
                    d.get("side", ""),
                    str(d.get("price", "")),
                    str(d.get("amount", "")),
                ]
                for d in data
            ]
            print_markdown_table(["Market", "Time", "Side", "Price", "Amount"], rows)
            return

        table = Table(title=f"Trades: {markets}")
        table.add_column("Market", style="cyan")
        table.add_column("Time", style="dim")
        table.add_column("Side", style="green")
        table.add_column("Price", style="yellow", justify="right")
        table.add_column("Amount", style="magenta", justify="right")

        for d in data:
            side = d.get("side", "")
            side_color = "green" if side == "buy" else "red" if side == "sell" else ""
            table.add_row(
                d.get("market", ""),
                d.get("time", "")[:19],
                f"[{side_color}]{side}[/]" if side_color else side,
                str(d.get("price", "")),
                str(d.get("amount", "")),
            )
    else:
        if markdown:
            rows = [
                [
                    d.get("market", ""),
                    d.get("time", "")[:19],
                    str(d.get("price_open", "")),
                    str(d.get("price_high", "")),
                    str(d.get("price_low", "")),
                    str(d.get("price_close", "")),
                    format_number(float(d.get("vwap", 0) or 0)),
                ]
                for d in data
            ]
            print_markdown_table(["Market", "Time", "Open", "High", "Low", "Close", "VWAP"], rows)
            return

        table = Table(title=f"Candles: {markets}")
        table.add_column("Market", style="cyan")
        table.add_column("Time", style="dim")
        table.add_column("Open", style="yellow", justify="right")
        table.add_column("High", style="green", justify="right")
        table.add_column("Low", style="red", justify="right")
        table.add_column("Close", style="yellow", justify="right")
        table.add_column("VWAP", style="magenta", justify="right")

        for d in data:
            table.add_row(
                d.get("market", ""),
                d.get("time", "")[:19],
                str(d.get("price_open", "")),
                str(d.get("price_high", "")),
                str(d.get("price_low", "")),
                str(d.get("price_close", "")),
                format_number(float(d.get("vwap", 0) or 0)),
            )

    console.print(table)


@app.command()
def exchanges(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """List available exchanges."""
    client = get_client()
    data = client.list_exchanges()[:limit]

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = [
            [e.get("exchange", ""), e.get("full_name", ""), e.get("exchange_type", "")]
            for e in data
        ]
        print_markdown_table(["Exchange", "Name", "Type"], rows)
        return

    table = Table(title="Available Exchanges")
    table.add_column("Exchange", style="cyan")
    table.add_column("Name", style="green", max_width=30)
    table.add_column("Type", style="yellow")

    for e in data:
        table.add_row(e.get("exchange", ""), e.get("full_name", ""), e.get("exchange_type", ""))

    console.print(table)


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /catalog/assets)"),
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
        data = client.raw_request(endpoint, params=query_params)
        print(json.dumps(data, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
