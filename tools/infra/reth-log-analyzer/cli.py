"""CLI for reth log analyzer."""

from dotenv import load_dotenv

load_dotenv()

from pathlib import Path
from typing import Optional

import json
import typer
from rich.console import Console

from centaur_sdk import Table

from .client import _client

app = typer.Typer(help="Parse reth logs and generate performance graphs")


@app.command("health")
def health():
    """Assert reth-log-analyzer connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {"auth_mode": "local-only", "live_probe": False}
        payload = {"ok": True, "tool": "reth-log-analyzer", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "reth-log-analyzer", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


@app.command()
def parse(
    log_file: Path = typer.Argument(..., help="Path to reth log file"),
    output: Optional[Path] = typer.Option(None, "-o", "--output", help="Output CSV file"),
    limit: int = typer.Option(0, "-n", "--limit", help="Limit number of blocks to show (0=all)"),
    min_gas: float = typer.Option(0.0, "--min-gas", help="Minimum gas in Mgas to include"),
):
    """Parse reth log file and display block metrics."""
    if not log_file.exists():
        console.print(f"[red]Error: File not found: {log_file}[/red]")
        raise typer.Exit(1)

    client = _client()
    df = client.parse(log_file, min_gas=min_gas)

    if df.empty:
        console.print("[yellow]No blocks found in log file[/yellow]")
        raise typer.Exit(0)

    console.print(f"[green]Parsed {len(df)} blocks[/green]")

    if output:
        df.to_csv(output, index=False)
        console.print(f"[green]Saved to {output}[/green]")
    else:
        display_df = df if limit == 0 else df.tail(limit)

        table = Table(title="Block Metrics")
        table.add_column("Block", justify="right")
        table.add_column("Txs", justify="right")
        table.add_column("Gas (Mgas)", justify="right")
        table.add_column("Throughput (Ggas/s)", justify="right")
        table.add_column("Latency (ms)", justify="right")
        table.add_column("State Root (ms)", justify="right")
        table.add_column("Exec %", justify="right")

        for _, row in display_df.iterrows():
            table.add_row(
                str(row["block_number"]),
                str(row["txs"]),
                f"{row['gas_used_mgas']:.1f}",
                f"{row['gas_throughput_ggas_s']:.2f}",
                f"{row['elapsed_ms']:.1f}",
                f"{row['state_root_ms']:.2f}",
                f"{row['execution_pct']:.1f}%",
            )

        console.print(table)

        if len(df) > 0:
            console.print("\n[bold]Summary:[/bold]")
            console.print(f"  Blocks: {len(df)}")
            console.print(f"  Avg throughput: {df['gas_throughput_ggas_s'].mean():.2f} Ggas/s")
            console.print(f"  Avg latency: {df['elapsed_ms'].mean():.1f} ms")
            console.print(f"  Avg execution %: {df['execution_pct'].mean():.1f}%")
            console.print(f"  Max latency: {df['elapsed_ms'].max():.1f} ms")
            console.print(f"  Max gas: {df['gas_used_mgas'].max():.1f} Mgas")


@app.command()
def graphs(
    log_file: Path = typer.Argument(..., help="Path to reth log file"),
    output_dir: Path = typer.Option(
        Path("."), "-o", "--output", help="Output directory for graphs"
    ),
    min_gas: float = typer.Option(0.0, "--min-gas", help="Minimum gas in Mgas to include"),
    title: str = typer.Option("", "--title", help="Title suffix for graphs"),
):
    """Generate performance graphs from reth log file."""
    if not log_file.exists():
        console.print(f"[red]Error: File not found: {log_file}[/red]")
        raise typer.Exit(1)

    client = _client()

    try:
        paths = client.generate_graphs(log_file, output_dir, min_gas=min_gas, title_suffix=title)
    except ValueError as e:
        console.print(f"[red]Error: {e}[/red]")
        raise typer.Exit(1)

    if not paths:
        console.print("[yellow]No blocks found in log file[/yellow]")
        raise typer.Exit(0)

    console.print(f"[green]Generated {len(paths)} graphs:[/green]")
    for p in paths:
        console.print(f"  - {p}")


@app.command()
def summary(
    log_file: Path = typer.Argument(..., help="Path to reth log file"),
    min_gas: float = typer.Option(10.0, "--min-gas", help="Minimum gas in Mgas for 'big blocks'"),
    markdown: bool = typer.Option(False, "-m", "--markdown", help="Output as markdown"),
):
    """Generate performance summary report."""
    if not log_file.exists():
        console.print(f"[red]Error: File not found: {log_file}[/red]")
        raise typer.Exit(1)

    client = _client()
    stats = client.summary(log_file, min_gas=min_gas)

    if not stats:
        console.print("[yellow]No blocks found in log file[/yellow]")
        raise typer.Exit(0)

    block_min, block_max = stats["block_range"]

    if markdown:
        print("## Reth Big Blocks Performance Analysis\n")
        print("### Dataset Overview")
        print(f"- **{stats['total_blocks']:,} blocks** analyzed ({block_min} - {block_max})")
        print(f"- {stats['empty_blocks']:,} empty blocks, {stats['non_empty_blocks']:,} non-empty")
        print(
            f"- Max gas: **{stats['max_gas_mgas']:.0f} Mgas** | Max latency: **{stats['max_latency_ms']:.0f}ms**\n"
        )

        category_labels = {
            "empty": "Empty",
            "light": "Light (<10M)",
            "medium": "Medium (10-50M)",
            "big": "Big (50-500M)",
            "huge": "Huge (>500M)",
        }

        print("### Block Categories")
        print("| Category | Blocks | Avg Latency | State Root % | Execution % |")
        print("|----------|--------|-------------|--------------|-------------|")

        for key, label in category_labels.items():
            cat = stats["categories"].get(key)
            if cat:
                print(
                    f"| {label} | {cat['count']:,} | {cat['avg_latency_ms']:.1f}ms | {cat['avg_state_root_pct']:.1f}% | **{cat['avg_execution_pct']:.1f}%** |"
                )

        big = stats.get("big_blocks")
        if big:
            print(f"\n### Key Findings (blocks >{big['min_gas_threshold']:.0f}M gas)")
            print(f"- **{big['count']} blocks** analyzed")
            print(f"- **Avg throughput:** {big['avg_throughput_ggas_s']:.2f} Ggas/s")
            print(f"- **Execution:** {big['avg_execution_pct']:.1f}% of total time")
            print(f"- **State root:** {big['avg_state_root_pct']:.1f}% of total time")
            print(
                f"- **Slowest block:** #{big['slowest_block']} at {big['slowest_latency_ms']:.0f}ms ({big['slowest_gas_mgas']:.0f}M gas)"
            )
    else:
        console.print("[bold]Reth Big Blocks Performance Analysis[/bold]\n")
        console.print(f"Blocks analyzed: {stats['total_blocks']:,} ({block_min} - {block_max})")
        console.print(
            f"Empty: {stats['empty_blocks']:,} | Non-empty: {stats['non_empty_blocks']:,}"
        )
        console.print(
            f"Max gas: {stats['max_gas_mgas']:.0f} Mgas | Max latency: {stats['max_latency_ms']:.0f}ms\n"
        )

        big = stats.get("big_blocks")
        if big:
            console.print(
                f"[bold]Big blocks (>{big['min_gas_threshold']:.0f}M gas): {big['count']}[/bold]"
            )
            console.print(f"  Avg throughput: {big['avg_throughput_ggas_s']:.2f} Ggas/s")
            console.print(f"  Avg execution %: {big['avg_execution_pct']:.1f}%")
            console.print(f"  Avg state root %: {big['avg_state_root_pct']:.1f}%")


if __name__ == "__main__":
    app()
