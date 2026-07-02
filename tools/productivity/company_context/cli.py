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

app = typer.Typer(
    name="company_context",
    help="Search indexed company history, Slack DMs, and Google Docs.",
)


@app.command("health")
def health():
    """Assert company-context connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.latest_date()
        if isinstance(details, dict) and details.get("status") == "error":
            raise RuntimeError(str(details.get("error") or "company-context health check failed"))
        payload = {"ok": True, "tool": "company-context", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "company-context", "error": str(exc), "details": {}}
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
    source: str | None = typer.Option(
        None,
        "--source",
        help="Filter by source. Use 'docs' for Google Docs.",
    ),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only results on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only results before this time."
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Search indexed company context documents, including Google Docs with --source docs."""
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


@app.command("search-dm-conversations")
def search_dm_conversations(
    query: str = typer.Argument(..., help="Person, user id, or conversation search query."),
    limit: int = typer.Option(10, "--limit", "-n", help="Max conversations."),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Find Slack DM/group DM conversations visible to the current user."""
    result = CompanyContextClient().search_dm_conversations(query=query, limit=limit)
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    results = result.get("results") or []
    if not results:
        console.print(f"[yellow]No Slack DM conversations found for: {query}[/yellow]")
        return

    table = Table(title=f"Slack DM Conversations ({len(results)})")
    table.add_column("Conversation", style="magenta", max_width=16)
    table.add_column("Type", style="cyan", max_width=10)
    table.add_column("Participants", style="bold", max_width=42)
    table.add_column("Matched", max_width=32)
    table.add_column("Last Seen", style="green", max_width=20)
    for item in results:
        table.add_row(
            str(item.get("conversation_id") or ""),
            str(item.get("conversation_type") or ""),
            ", ".join(str(label) for label in item.get("participant_labels") or []),
            ", ".join(str(label) for label in item.get("matched_labels") or []),
            str(item.get("last_seen_at") or ""),
        )
    console.print(table)


@app.command("search-dms")
def search_dms(
    query: str = typer.Argument(..., help="Search query."),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results."),
    conversation_id: str | None = typer.Option(
        None,
        "--conversation-id",
        help="Filter to one Slack DM/MPIM conversation id.",
    ),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only results on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only results before this time."
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Search Slack DMs and group DMs visible to the current user."""
    result = CompanyContextClient().search_dms(
        query=query,
        limit=limit,
        conversation_id=conversation_id,
        occurred_after=occurred_after,
        occurred_before=occurred_before,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    results = result.get("results") or []
    if not results:
        console.print(f"[yellow]No Slack DMs found for: {query}[/yellow]")
        return

    table = Table(title=f"Slack DM Search ({len(results)})")
    table.add_column("Document ID", style="dim", max_width=40)
    table.add_column("Conversation", style="magenta", max_width=16)
    table.add_column("Type", style="cyan", max_width=10)
    table.add_column("Occurred", style="green", max_width=20)
    table.add_column("Title", style="bold", max_width=24)
    table.add_column("Preview", max_width=72)
    for item in results:
        table.add_row(
            str(item.get("document_id") or ""),
            str(item.get("conversation_id") or ""),
            str(item.get("conversation_type") or ""),
            str(item.get("occurred_at") or ""),
            str(item.get("title") or ""),
            str(item.get("preview") or ""),
        )
    console.print(table)


@app.command("list")
def list_documents(
    limit: int = typer.Option(10, "--limit", "-n", help="Max documents."),
    source: str | None = typer.Option(
        None,
        "--source",
        help="Filter by source. Use 'docs' for Google Docs.",
    ),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only documents on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only documents before this time."
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """List indexed company context documents, including Google Docs with --source docs."""
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
    related: bool = typer.Option(
        False, "--related", help="Include parent/child document summaries."
    ),
    max_related_children: int = typer.Option(
        10, "--max-related-children", help="Max related children."
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON."),
) -> None:
    """Read a company context document returned by search, including Google Docs chunks."""
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
    source: str | None = typer.Option(
        None,
        "--source",
        help="Filter by source. Use 'docs' for Google Docs.",
    ),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
) -> None:
    """Show the latest indexed timestamp."""
    result = CompanyContextClient().latest_date(source=source, source_type=source_type)
    _require_ok(result)
    _print_json(result)


if __name__ == "__main__":
    app()
