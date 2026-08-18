"""CLI for Centaur readonly thread investigations."""

from __future__ import annotations

import json
from typing import Any

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.json import JSON
from rich.table import Table

from .client import CentaurInvestigatorClient

load_dotenv()

app = typer.Typer(
    name="centaur-investigator",
    help="Investigate Centaur threads and sessions from readonly Postgres.",
)


@app.command("health")
def health():
    """Assert centaur-investigator connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search_sessions(limit=1)
        if isinstance(details, dict) and details.get("status") == "error":
            raise RuntimeError(
                str(details.get("error") or "centaur-investigator health check failed")
            )
        payload = {"ok": True, "tool": "centaur-investigator", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "centaur-investigator", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def _print_json(data: dict[str, Any]) -> None:
    console.print(JSON(json.dumps(data, default=str)))


def _require_ok(result: dict[str, Any]) -> None:
    if result.get("status") == "error":
        console.print(f"[red]{result.get('error', 'unknown error')}[/red]")
        raise typer.Exit(1)


def _print_investigation(result: dict[str, Any]) -> None:
    parsed = result.get("parsed") or {}
    analysis = result.get("analysis") or {}
    console.print(
        f"[bold]Thread:[/] {parsed.get('thread_key') or ', '.join(result.get('thread_keys') or [])}"
    )
    if parsed.get("permalink"):
        console.print(f"[dim]{parsed['permalink']}[/dim]")
    console.print(analysis.get("summary") or "No summary.")

    warnings = analysis.get("warnings") or []
    for warning in warnings:
        console.print(f"[yellow]warning:[/] {warning}")

    executions = (result.get("postgres") or {}).get("session_executions", {}).get("rows", [])
    if executions:
        table = Table(title="Executions")
        table.add_column("Execution", style="cyan", max_width=28)
        table.add_column("Status", style="green", max_width=14)
        table.add_column("Created", max_width=24)
        table.add_column("Started", max_width=24)
        table.add_column("Completed", max_width=24)
        for row in executions[:10]:
            table.add_row(
                str(row.get("execution_id") or ""),
                str(row.get("status") or ""),
                str(row.get("created_at") or ""),
                str(row.get("started_at") or ""),
                str(row.get("completed_at") or ""),
            )
        console.print(table)


@app.command("investigate")
def investigate(
    query: str = typer.Argument(..., help="Natural-language query, Slack link, or thread_key."),
    limit: int = typer.Option(25, "--limit", "-n", help="Max rows per source."),
    observability: bool = typer.Option(
        True,
        "--observability/--no-observability",
        help="Query vlogs/vmetrics.",
    ),
    window_hours: int = typer.Option(24, "--window-hours", help="Observability lookback."),
    logs_limit: int = typer.Option(100, "--logs-limit", help="Max log rows."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Investigate a Slack thread or Centaur thread key."""
    result = CentaurInvestigatorClient().investigate(
        query,
        limit=limit,
        include_observability=observability,
        window_hours=window_hours,
        logs_limit=logs_limit,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return
    _print_investigation(result)


@app.command("slack-thread")
def slack_thread(
    reference: str = typer.Argument(..., help="Slack permalink or Slack thread_key."),
    limit: int = typer.Option(25, "--limit", "-n", help="Max rows per source."),
    observability: bool = typer.Option(
        True,
        "--observability/--no-observability",
        help="Query vlogs/vmetrics.",
    ),
    window_hours: int = typer.Option(24, "--window-hours", help="Observability lookback."),
    logs_limit: int = typer.Option(100, "--logs-limit", help="Max log rows."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Investigate a Slack thread."""
    result = CentaurInvestigatorClient().investigate_slack_thread(
        reference,
        limit=limit,
        include_observability=observability,
        window_hours=window_hours,
        logs_limit=logs_limit,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return
    _print_investigation(result)


@app.command("session")
def session(
    thread_key: str = typer.Argument(..., help="Centaur thread_key."),
    limit: int = typer.Option(25, "--limit", "-n", help="Max rows per source."),
    observability: bool = typer.Option(
        True,
        "--observability/--no-observability",
        help="Query vlogs/vmetrics.",
    ),
    window_hours: int = typer.Option(24, "--window-hours", help="Observability lookback."),
    logs_limit: int = typer.Option(100, "--logs-limit", help="Max log rows."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Inspect source-of-truth state for a thread_key."""
    result = CentaurInvestigatorClient().session_state(
        thread_key,
        limit=limit,
        include_observability=observability,
        window_hours=window_hours,
        logs_limit=logs_limit,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return
    _print_investigation(result)


@app.command("parse")
def parse(
    reference: str = typer.Argument(..., help="Slack link or thread_key."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Parse a Slack reference without querying Postgres."""
    result = CentaurInvestigatorClient().parse_thread_reference(reference)
    _require_ok(result)
    if json_output:
        _print_json(result)
        return
    console.print(f"[bold]Channel:[/] {result.get('channel_id')}")
    console.print(f"[bold]Thread TS:[/] {result.get('thread_ts')}")
    console.print("[bold]Candidates:[/]")
    for candidate in result.get("thread_key_candidates") or []:
        console.print(f"  {candidate}")


@app.command("search-sessions")
def search_sessions(
    query: str = typer.Option("", "--query", "-q", help="Thread key substring."),
    channel_id: str = typer.Option("", "--channel", help="Slack channel id."),
    status: str = typer.Option("", "--status", help="Session status."),
    limit: int = typer.Option(25, "--limit", "-n", help="Max rows."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Search sessions by thread key, channel, or status."""
    result = CentaurInvestigatorClient().search_sessions(
        query=query,
        channel_id=channel_id,
        status=status,
        limit=limit,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    table = Table(title=f"Sessions ({result.get('count', 0)})")
    table.add_column("Thread Key", style="cyan", max_width=64)
    table.add_column("Harness", max_width=12)
    table.add_column("Status", style="green", max_width=14)
    table.add_column("Updated", max_width=24)
    for row in result.get("sessions") or []:
        table.add_row(
            str(row.get("thread_key") or ""),
            str(row.get("harness_type") or ""),
            str(row.get("status") or ""),
            str(row.get("updated_at") or ""),
        )
    console.print(table)


if __name__ == "__main__":
    app()
