"""CLI for Dune Analytics API."""

import json
import time

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.status import Status
from rich.table import Table

from .client import DuneClient

load_dotenv()

_client: DuneClient | None = None


def _get_client() -> DuneClient:
    global _client
    if _client is None:
        _client = DuneClient()
    return _client


app = typer.Typer(name="dune", help="Dune Analytics CLI for executing queries and fetching results")


@app.command("health")
def health():
    """Assert dune connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.raw_request("GET", "/query/2408388/results")
        payload = {"ok": True, "tool": "dune", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "dune", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


@app.command()
def execute(
    query_id: int = typer.Argument(..., help="Dune query ID"),
    params: str = typer.Option(None, "--params", "-p", help="Query parameters as JSON"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Execute a query and return the execution ID."""
    query_params = None
    if params:
        try:
            query_params = json.loads(params)
        except json.JSONDecodeError as e:
            console.print(f"[red]Invalid JSON for params: {e}[/]")
            raise typer.Exit(1)

    try:
        result = _get_client().execute_query(query_id, query_params)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    console.print(f"[green]Query {query_id} execution started[/]")
    console.print(f"Execution ID: [cyan]{result.get('execution_id')}[/]")
    console.print(f"State: {result.get('state')}")


@app.command()
def status(
    execution_id: str = typer.Argument(..., help="Execution ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Check the status of an execution."""
    try:
        result = _get_client().get_execution_status(execution_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    state = result.get("state", "unknown")
    state_color = {
        "QUERY_STATE_PENDING": "yellow",
        "QUERY_STATE_EXECUTING": "blue",
        "QUERY_STATE_COMPLETED": "green",
        "QUERY_STATE_FAILED": "red",
        "QUERY_STATE_CANCELLED": "dim",
    }.get(state, "white")

    console.print(f"Execution ID: [cyan]{execution_id}[/]")
    console.print(f"State: [{state_color}]{state}[/]")

    if queue_pos := result.get("queue_position"):
        console.print(f"Queue Position: {queue_pos}")


@app.command()
def results(
    execution_id: str = typer.Argument(..., help="Execution ID"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max rows to display"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get results of a completed execution."""
    try:
        result = _get_client().get_execution_results(execution_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    state = result.get("state")
    if state != "QUERY_STATE_COMPLETED":
        console.print(f"[yellow]Execution not complete. State: {state}[/]")
        raise typer.Exit(1)

    result_data = result.get("result", {})
    rows = result_data.get("rows", [])
    metadata = result_data.get("metadata", {})

    if not rows:
        console.print("[yellow]No results[/]")
        return

    columns = metadata.get("column_names", list(rows[0].keys()) if rows else [])

    table = Table(title=f"Results ({len(rows)} rows)")
    for col in columns:
        table.add_column(col, overflow="fold", max_width=40)

    for row in rows[:limit]:
        table.add_row(*[str(row.get(col, "")) for col in columns])

    console.print(table)

    if len(rows) > limit:
        console.print(f"[dim]... showing {limit} of {len(rows)} rows[/]")


@app.command()
def run(
    query_id: int = typer.Argument(..., help="Dune query ID"),
    params: str = typer.Option(None, "--params", "-p", help="Query parameters as JSON"),
    poll_interval: float = typer.Option(2.0, "--poll", help="Poll interval in seconds"),
    timeout: float = typer.Option(300.0, "--timeout", "-t", help="Timeout in seconds"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max rows to display"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Execute a query and wait for results."""
    query_params = None
    if params:
        try:
            query_params = json.loads(params)
        except json.JSONDecodeError as e:
            console.print(f"[red]Invalid JSON for params: {e}[/]")
            raise typer.Exit(1)

    try:
        exec_result = _get_client().execute_query(query_id, query_params)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    execution_id = exec_result.get("execution_id")
    if not execution_id:
        console.print("[red]No execution ID returned[/]")
        raise typer.Exit(1)

    start_time = time.time()

    with Status(f"[bold blue]Executing query {query_id}...", console=console) as status_spinner:
        while True:
            elapsed = time.time() - start_time
            if elapsed > timeout:
                console.print(f"[red]Timeout after {timeout}s[/]")
                raise typer.Exit(1)

            try:
                status_result = _get_client().get_execution_status(execution_id)
            except RuntimeError as e:
                console.print(f"[red]Error checking status: {e}[/]")
                raise typer.Exit(1)

            state = status_result.get("state", "unknown")

            if state == "QUERY_STATE_COMPLETED":
                status_spinner.update("[bold green]Complete!")
                break
            elif state == "QUERY_STATE_FAILED":
                console.print("[red]Query execution failed[/]")
                if json_output:
                    print(json.dumps(status_result, indent=2))
                raise typer.Exit(1)
            elif state == "QUERY_STATE_CANCELLED":
                console.print("[yellow]Query was cancelled[/]")
                raise typer.Exit(1)

            queue_pos = status_result.get("queue_position", "")
            queue_info = f" (queue: {queue_pos})" if queue_pos else ""
            status_spinner.update(f"[bold blue]{state}{queue_info} ({elapsed:.0f}s)")

            time.sleep(poll_interval)

    try:
        final_result = _get_client().get_execution_results(execution_id)
    except RuntimeError as e:
        console.print(f"[red]Error fetching results: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(final_result, indent=2))
        return

    result_data = final_result.get("result", {})
    rows = result_data.get("rows", [])
    metadata = result_data.get("metadata", {})

    if not rows:
        console.print("[yellow]No results[/]")
        return

    columns = metadata.get("column_names", list(rows[0].keys()) if rows else [])

    table = Table(title=f"Query {query_id} Results ({len(rows)} rows)")
    for col in columns:
        table.add_column(col, overflow="fold", max_width=40)

    for row in rows[:limit]:
        table.add_row(*[str(row.get(col, "")) for col in columns])

    console.print(table)

    if len(rows) > limit:
        console.print(f"[dim]... showing {limit} of {len(rows)} rows[/]")


@app.command()
def cancel(
    execution_id: str = typer.Argument(..., help="Execution ID to cancel"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Cancel a running execution."""
    try:
        result = _get_client().cancel_execution(execution_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    console.print(f"[green]Execution {execution_id} cancelled[/]")


@app.command()
def query(
    query_id: int = typer.Argument(..., help="Dune query ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get query metadata."""
    try:
        result = _get_client().get_query(query_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    console.print(f"\n[bold cyan]{result.get('name', f'Query {query_id}')}[/]")

    if desc := result.get("description"):
        console.print(f"[dim]{desc[:200]}[/]")

    console.print()
    console.print(f"ID: {result.get('query_id')}")
    console.print(f"Owner: {result.get('owner', 'unknown')}")

    if params := result.get("parameters"):
        console.print("\n[bold]Parameters:[/]")
        for p in params:
            console.print(f"  - {p.get('key')}: {p.get('type')} = {p.get('value')}")


@app.command()
def raw(
    endpoint: str = typer.Argument(..., help="API endpoint (e.g., /query/123)"),
    method: str = typer.Option("GET", "--method", "-X", help="HTTP method"),
    data: str = typer.Option(None, "--data", "-d", help="Request body as JSON"),
):
    """Make a raw API call."""
    kwargs = {}
    if data:
        try:
            kwargs["json"] = json.loads(data)
        except json.JSONDecodeError as e:
            console.print(f"[red]Invalid JSON: {e}[/]")
            raise typer.Exit(1)

    try:
        result = _get_client().raw_request(method.upper(), endpoint, **kwargs)
        print(json.dumps(result, indent=2))
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
