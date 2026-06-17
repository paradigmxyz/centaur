"""CLI for privacy-safe Centaur thread investigations."""

from __future__ import annotations

import json
from typing import Any

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.json import JSON

from .client import CentaurInvestigatorClient

load_dotenv()

app = typer.Typer(
    name="centaur-investigator",
    help="Investigate Centaur threads without exposing message context.",
)
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
    console.print(f"[bold]Thread:[/] {parsed.get('thread_key') or ', '.join(result.get('thread_keys') or [])}")
    if parsed.get("permalink"):
        console.print(f"[dim]{parsed['permalink']}[/dim]")
    console.print(analysis.get("summary") or "No summary.")

    warnings = analysis.get("warnings") or []
    for warning in warnings:
        console.print(f"[yellow]warning:[/] {warning}")


@app.command("investigate")
def investigate(
    query: str = typer.Argument(..., help="Natural-language query, Slack link, or thread_key."),
    limit: int = typer.Option(25, "--limit", "-n", help="Max rows per source."),
    observability: bool = typer.Option(True, "--observability/--no-observability", help="Query vlogs/vmetrics."),
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
    observability: bool = typer.Option(True, "--observability/--no-observability", help="Query vlogs/vmetrics."),
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
    observability: bool = typer.Option(True, "--observability/--no-observability", help="Query vlogs/vmetrics."),
    window_hours: int = typer.Option(24, "--window-hours", help="Observability lookback."),
    logs_limit: int = typer.Option(100, "--logs-limit", help="Max log rows."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Inspect identifiers and observability for a thread_key."""
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


if __name__ == "__main__":
    app()
