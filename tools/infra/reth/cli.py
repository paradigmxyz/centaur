"""CLI for Reth execution timings and performance metrics."""

import json
from typing import Optional

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="reth", help="Reth CLI for execution timings and performance metrics")


@app.command("health")
def health():
    """Assert reth connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_execution_timings(hours=1, page_size=1)
        payload = {"ok": True, "tool": "reth", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "reth", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


@app.command()
def timings(
    duration: Optional[str] = typer.Argument(
        None, help="Time range: 1h, 6hr, 24hr, 7d (default: 1h)"
    ),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
    slack: bool = typer.Option(False, "--slack", "-s", help="Format for Slack"),
):
    """Show EL client execution timings from ethPandaOps."""
    import json as json_module

    from .client import RethClient

    client = RethClient()

    hours = 1
    duration_label = "1h"

    if duration:
        seconds = client.parse_duration(duration)
        hours = max(1, seconds // 3600)
        duration_label = duration

    page_size = min(5000, max(500, hours * 300))
    payloads = client.get_execution_timings(hours=hours, page_size=page_size)

    if not payloads:
        console.print("[yellow]No payload data found.[/]")
        raise typer.Exit()

    stats = client.aggregate_timings(payloads)

    if json_output:
        print(json_module.dumps(stats, indent=2))
        raise typer.Exit()

    if slack:
        lines = [f"*EL Client Timings (last {duration_label}):*"]
        for i, s in enumerate(stats):
            emoji = "🥇" if i == 0 else "🥈" if i == 1 else "🥉" if i == 2 else "•"
            lines.append(
                f"{emoji} *{s['client']}*: avg={s['avg_ms']}ms p50={s['p50_ms']}ms "
                f"p99={s['p99_ms']}ms (n={s['count']})"
            )
        print("\n".join(lines))
        raise typer.Exit()

    table = Table(title=f"EL Client Execution Timings (last {duration_label})")
    table.add_column("#", style="dim", justify="right", width=3)
    table.add_column("Client", style="cyan", max_width=15)
    table.add_column("Avg", style="green", justify="right")
    table.add_column("P50", justify="right")
    table.add_column("P90", justify="right")
    table.add_column("P99", justify="right")
    table.add_column("Min", style="dim", justify="right")
    table.add_column("Max", style="dim", justify="right")
    table.add_column("n", style="dim", justify="right")

    for i, s in enumerate(stats, 1):
        style = "bold green" if s["client"] == "Reth" else None
        table.add_row(
            str(i),
            s["client"],
            f"{s['avg_ms']}ms",
            f"{s['p50_ms']}ms",
            f"{s['p90_ms']}ms",
            f"{s['p99_ms']}ms",
            f"{s['min_ms']}ms",
            f"{s['max_ms']}ms",
            str(s["count"]),
            style=style,
        )

    console.print(table)

    reth_stats = next((s for s in stats if s["client"] == "Reth"), None)
    if reth_stats:
        rank = next(i for i, s in enumerate(stats, 1) if s["client"] == "Reth")
        if rank == 1:
            console.print("\n[bold green]🏆 Reth is #1![/]")
        else:
            leader = stats[0]
            diff = reth_stats["avg_ms"] - leader["avg_ms"]
            console.print(f"\n[yellow]Reth is #{rank}, {diff}ms behind {leader['client']}[/]")


@app.command()
def slow(
    duration: Optional[str] = typer.Argument(None, help="Time range (default: 24hr)"),
    threshold: int = typer.Option(500, "--threshold", "-t", help="Min duration in ms"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
):
    """Show slow execution payloads."""
    from .client import RethClient

    client = RethClient()

    hours = 24
    if duration:
        hours = max(1, client.parse_duration(duration) // 3600)

    payloads = client.get_execution_timings(hours=hours, page_size=1000)
    slow_payloads = [p for p in payloads if p.get("duration_ms", 0) >= threshold]
    slow_payloads.sort(key=lambda x: x.get("duration_ms", 0), reverse=True)
    slow_payloads = slow_payloads[:limit]

    if not slow_payloads:
        console.print(f"[green]No payloads >{threshold}ms found.[/]")
        raise typer.Exit()

    table = Table(title=f"Slow Payloads (>{threshold}ms)")
    table.add_column("Duration", style="red", justify="right")
    table.add_column("Client", style="cyan")
    table.add_column("Block", justify="right")
    table.add_column("Gas", justify="right")
    table.add_column("Txs", justify="right")

    for p in slow_payloads:
        cl = p.get("meta_execution_implementation", "?")
        is_reth = cl == "Reth"
        style = "bold red" if is_reth else None
        table.add_row(
            f"{p.get('duration_ms', 0)}ms",
            cl,
            str(p.get("block_number", "")),
            f"{p.get('gas_used', 0):,}",
            str(p.get("tx_count", "")),
            style=style,
        )

    console.print(table)


if __name__ == "__main__":
    app()
