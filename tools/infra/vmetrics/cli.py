"""CLI for VictoriaMetrics queries."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

load_dotenv()

app = typer.Typer(name="vmetrics", help="VictoriaMetrics CLI for PromQL/MetricsQL queries")
console = Console()


def get_client():
    from .client import VictoriaMetricsClient

    return VictoriaMetricsClient()


@app.command("query")
def query_metrics(
    expr: str = typer.Argument(..., help="PromQL/MetricsQL expression"),
    time: str = typer.Option(None, "--time", "-t", help="Evaluation timestamp"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Run an instant query."""
    result = get_client().query(expr=expr, time=time)
    if json_output:
        print(json.dumps(result, indent=2))
        return
    console.print_json(json.dumps(result))


@app.command("query-range")
def query_range(
    expr: str = typer.Argument(..., help="PromQL/MetricsQL expression"),
    start: str = typer.Argument(..., help="Range start timestamp"),
    end: str = typer.Option(None, "--end", "-e", help="Range end timestamp"),
    step: str = typer.Option("60s", "--step", "-s", help="Query step"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Run a range query."""
    result = get_client().query_range(expr=expr, start=start, end=end, step=step)
    if json_output:
        print(json.dumps(result, indent=2))
        return
    console.print_json(json.dumps(result))


@app.command("series")
def series(
    match: str = typer.Argument(..., help='Series selector, e.g. {__name__=~"agent_.*"}'),
    start: str = typer.Option(None, "--start", "-s", help="Range start timestamp"),
    end: str = typer.Option(None, "--end", "-e", help="Range end timestamp"),
    limit: int | None = typer.Option(None, "--limit", "-n", help="Max returned series"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Find matching time series."""
    result = get_client().series(match=match, start=start, end=end, limit=limit)
    if json_output:
        print(json.dumps(result, indent=2))
        return
    console.print_json(json.dumps(result))


@app.command("label-values")
def label_values(
    label: str = typer.Argument(..., help="Label name, e.g. __name__"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List values for a label."""
    result = get_client().label_values(label)
    if json_output:
        print(json.dumps(result, indent=2))
        return
    for value in result:
        console.print(value)


@app.command("metric-names")
def metric_names(
    prefix: str = typer.Option("agent_", "--prefix", "-p", help="Only include names with prefix"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List metric names."""
    result = get_client().metric_names(prefix=prefix)
    if json_output:
        print(json.dumps(result, indent=2))
        return
    for name in result:
        console.print(name)


@app.command()
def health():
    """Assert VictoriaMetrics readiness and basic query functionality."""
    payload = get_client().health()
    print(json.dumps(payload, indent=2, default=str))
    if not payload.get("ok"):
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
