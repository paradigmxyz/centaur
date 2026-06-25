"""Company context CLI for AI agents."""

from __future__ import annotations

import json
from typing import Any

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.json import JSON
from rich.table import Table

from .client import CompanyContextClient

load_dotenv()

app = typer.Typer(name="company_context", help="Search indexed company history.")
console = Console()


def _print_json(data: dict[str, Any]) -> None:
    console.print(JSON(json.dumps(data, default=str)))


def _require_ok(result: dict[str, Any]) -> None:
    if result.get("status") == "error":
        console.print(f"[red]{result.get('error', 'unknown error')}[/red]")
        raise typer.Exit(1)


def _add_result_rows(table: Table, results: list[dict[str, Any]]) -> None:
    for item in results:
        table.add_row(
            str(item.get("document_id") or ""),
            str(item.get("source") or ""),
            str(item.get("source_type") or ""),
            str(item.get("occurred_at") or ""),
            str(item.get("title") or ""),
            str(item.get("preview") or ""),
        )


@app.command("search")
def search(
    query: str = typer.Argument(..., help="Search query."),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results."),
    source: str | None = typer.Option(None, "--source", help="Filter by source."),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
    occurred_after: str | None = typer.Option(None, "--after", help="Only results on/after this time."),
    occurred_before: str | None = typer.Option(None, "--before", help="Only results before this time."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Search indexed company context documents."""
    result = CompanyContextClient().search(
        query=query,
        limit=limit,
        source=source,
        source_type=source_type,
        occurred_after=occurred_after,
        occurred_before=occurred_before,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    results = result.get("results") or []
    if not results:
        console.print(f"[yellow]No company context found for: {query}[/yellow]")
        return

    table = Table(title=f"Company Context Search ({len(results)})")
    table.add_column("Document ID", style="dim", max_width=36)
    table.add_column("Source", style="magenta", max_width=12)
    table.add_column("Type", style="cyan", max_width=18)
    table.add_column("Occurred", style="green", max_width=20)
    table.add_column("Title", style="bold", max_width=36)
    table.add_column("Preview", max_width=72)
    _add_result_rows(table, results)
    console.print(table)


@app.command("list")
def list_documents(
    limit: int = typer.Option(10, "--limit", "-n", help="Max documents."),
    source: str | None = typer.Option(None, "--source", help="Filter by source."),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
    occurred_after: str | None = typer.Option(None, "--after", help="Only documents on/after this time."),
    occurred_before: str | None = typer.Option(None, "--before", help="Only documents before this time."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """List indexed company context documents."""
    result = CompanyContextClient().list_documents(
        limit=limit,
        source=source,
        source_type=source_type,
        occurred_after=occurred_after,
        occurred_before=occurred_before,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    results = result.get("results") or []
    if not results:
        console.print("[yellow]No company context documents found.[/yellow]")
        return

    table = Table(title=f"Company Context Documents ({len(results)})")
    table.add_column("Document ID", style="dim", max_width=36)
    table.add_column("Source", style="magenta", max_width=12)
    table.add_column("Type", style="cyan", max_width=18)
    table.add_column("Occurred", style="green", max_width=20)
    table.add_column("Title", style="bold", max_width=36)
    table.add_column("Preview", max_width=72)
    _add_result_rows(table, results)
    console.print(table)


@app.command("read")
def read_document(
    document_id: str = typer.Argument(..., help="Document ID returned by search/list."),
    max_chars: int = typer.Option(0, "--max-chars", help="Maximum content chars; 0 means full."),
    related: bool = typer.Option(False, "--related", help="Include parent/child document summaries."),
    max_related_children: int = typer.Option(10, "--max-related-children", help="Max related children."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Read a company context document."""
    result = CompanyContextClient().read_document(
        document_id=document_id,
        max_chars=max_chars,
        include_related=related,
        max_related_children=max_related_children,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    title = result.get("title") or result.get("document_id") or "Company Context Document"
    console.print(f"[bold]{title}[/bold]")
    if result.get("url"):
        console.print(f"[dim]{result['url']}[/dim]")
    console.print(result.get("content") or "")
    if result.get("truncated"):
        console.print(
            f"[yellow]Truncated at {result.get('chars')} of {result.get('total_chars')} chars.[/yellow]"
        )


@app.command("latest-date")
def latest_date(
    source: str | None = typer.Option(None, "--source", help="Filter by source."),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
) -> None:
    """Show the latest indexed timestamp."""
    result = CompanyContextClient().latest_date(source=source, source_type=source_type)
    _require_ok(result)
    _print_json(result)


if __name__ == "__main__":
    app()
