"""CLI for VictoriaLogs queries."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from centaur_sdk import Table

load_dotenv()

app = typer.Typer(name="vlogs", help="VictoriaLogs CLI for LogsQL queries and log exploration")
console = Console()


def get_client():
    from .client import VictoriaLogsClient

    return VictoriaLogsClient()


@app.command("query")
def query_logs(
    query: str = typer.Argument(..., help="LogsQL expression (e.g. '_time:5m error')"),
    start: str = typer.Option(None, "--start", "-s", help="Range start (RFC3339 or epoch)"),
    end: str = typer.Option(None, "--end", "-e", help="Range end"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max log lines"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Run a LogsQL query against VictoriaLogs."""
    client = get_client()
    result = client.query(query=query, limit=limit, start=start, end=end)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    if not result:
        console.print("[yellow]No results[/]")
        return

    for entry in result:
        stream = entry.get("_stream", "")
        time = entry.get("_time", "")
        msg = entry.get("_msg", "")
        console.print(f"[dim]{time}[/] [cyan]{stream}[/] {msg}")


@app.command("fields")
def list_fields(
    query: str = typer.Option("*", "--query", "-q", help="LogsQL filter"),
    start: str = typer.Option(None, "--start", "-s", help="Start time filter"),
    end: str = typer.Option(None, "--end", "-e", help="End time filter"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List all known field names."""
    client = get_client()
    result = client.field_names(query=query, start=start, end=end)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    for field in result:
        console.print(f"  {field}")


@app.command("field-values")
def field_values(
    field: str = typer.Argument(..., help="Field name (e.g. service, container)"),
    query: str = typer.Option("*", "--query", "-q", help="LogsQL filter"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max values"),
    start: str = typer.Option(None, "--start", "-s", help="Start time filter"),
    end: str = typer.Option(None, "--end", "-e", help="End time filter"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get all values for a field."""
    client = get_client()
    result = client.field_values(field, query=query, limit=limit, start=start, end=end)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    for value in result:
        console.print(f"  {value}")


@app.command("streams")
def list_streams(
    query: str = typer.Option("*", "--query", "-q", help="LogsQL filter"),
    start: str = typer.Option(None, "--start", "-s", help="Start time filter"),
    end: str = typer.Option(None, "--end", "-e", help="End time filter"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Find log streams matching a query."""
    client = get_client()
    result = client.streams(query=query, start=start, end=end)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    if not result:
        console.print("[yellow]No streams found[/]")
        return

    table = Table(title="Streams")
    table.add_column("Stream", style="cyan")
    table.add_column("Hits", style="green")
    for s in result:
        table.add_row(str(s.get("value", "")), str(s.get("hits", "")))

    console.print(table)


@app.command()
def health():
    """Assert VictoriaLogs readiness."""
    client = get_client()
    ready = client.ready()
    payload = {
        "ok": ready,
        "tool": "vlogs",
        "error": None if ready else "VictoriaLogs is not ready",
        "details": {"ready": ready},
    }
    print(json.dumps(payload, indent=2, default=str))
    if not ready:
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
