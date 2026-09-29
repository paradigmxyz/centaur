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
from .drive_v2 import DriveV2Client

load_dotenv()

app = typer.Typer(
    name="company_context",
    help=(
        "Search or run scoped SQL over company history, Slack DMs, Google Docs, and "
        "Granola notes. Search for and read the `company-context` skill with "
        "`centaur-skills` before use.\n\n"
        "Preview: `company_context v2 search|list|read|latest-date` query only Google "
        "Docs and PDFs from the company-context service's Drive index. Run "
        "`company_context v2 status` to check whether that index is available."
    ),
)
v2_app = typer.Typer(
    help=(
        "Preview: query Google Docs and PDFs from the company-context service's Drive "
        "index. Results cover only that index; use the top-level commands for other "
        "sources. Run `company_context v2 status` first."
    ),
)
app.add_typer(v2_app, name="v2")


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


@app.command("query")
def query(
    sql: str = typer.Argument(..., help="Read-only SQL query to execute."),
    limit: int = typer.Option(100, "--limit", "-n", help="Maximum rows to return."),
    timeout_seconds: int = typer.Option(
        10,
        "--timeout-seconds",
        help="Query timeout in seconds, capped at 30.",
    ),
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """Run raw read-only SQL against the scoped company-context database."""
    result = CompanyContextClient().query(
        sql=sql,
        limit=limit,
        timeout_seconds=timeout_seconds,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return

    rows = result.get("rows") or []
    columns = result.get("columns") or []
    if not rows:
        console.print("[yellow]Query returned no rows.[/yellow]")
        return

    table = Table(title=f"Company Context Query ({len(rows)})")
    for column in columns:
        table.add_column(str(column), overflow="fold")
    for row in rows:
        table.add_row(
            *[
                json.dumps(row.get(column), default=str)
                if isinstance(row.get(column), (dict, list))
                else str(row.get(column))
                for column in columns
            ]
        )
    console.print(table)
    if result.get("truncated"):
        console.print(f"[yellow]Results truncated at {result.get('limit')} rows.[/yellow]")


@app.command("search")
def search(
    query: str = typer.Argument(..., help="Search query."),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results."),
    source: str | None = typer.Option(
        None,
        "--source",
        help="Filter by source. Use 'docs' for Google Docs or 'granola' for Granola notes.",
    ),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only results on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only results before this time."
    ),
    hybrid: bool = typer.Option(
        True,
        "--hybrid/--no-hybrid",
        help="Fuse keyword and vector results when embeddings are enabled.",
    ),
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """Search indexed company context, including Google Docs and Granola notes."""
    result = CompanyContextClient().search(
        query=query,
        limit=limit,
        source=source,
        source_type=source_type,
        occurred_after=occurred_after,
        occurred_before=occurred_before,
        hybrid=hybrid,
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
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
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
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
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
        help="Filter by source. Use 'docs' for Google Docs or 'granola' for Granola notes.",
    ),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only documents on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only documents before this time."
    ),
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """List indexed company context documents, including Google Docs and Granola notes."""
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
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """Read a company context document returned by search, including Granola notes."""
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
        help="Filter by source. Use 'docs' for Google Docs or 'granola' for Granola notes.",
    ),
    source_type: str | None = typer.Option(None, "--source-type", help="Filter by source type."),
) -> None:
    """Show the latest indexed timestamp as JSON."""
    result = CompanyContextClient().latest_date(source=source, source_type=source_type)
    _require_ok(result)
    _print_json(result)


def _print_documents_table(title: str, results: list[dict[str, Any]]) -> None:
    table = Table(title=title)
    table.add_column("Document ID", style="dim", max_width=36)
    table.add_column("Type", style="cyan", max_width=12)
    table.add_column("Updated", style="green", max_width=20)
    table.add_column("Title", style="bold", max_width=36)
    table.add_column("Preview", max_width=72)
    for item in results:
        table.add_row(
            str(item.get("document_id") or ""),
            str(item.get("source_type") or ""),
            str(item.get("source_updated_at") or ""),
            str(item.get("title") or ""),
            str(item.get("preview") or ""),
        )
    console.print(table)


_V2_SOURCE_TYPE_HELP = "Filter by document type: google_doc or pdf."


@v2_app.command("status")
def v2_status() -> None:
    """Show whether the Drive index is readable, as JSON."""
    result = DriveV2Client().status()
    _require_ok(result)
    _print_json(result)


@v2_app.command("search")
def v2_search(
    query: str = typer.Argument(..., help="Search query."),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results."),
    source_type: str | None = typer.Option(None, "--source-type", help=_V2_SOURCE_TYPE_HELP),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only documents modified on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only documents modified before this time."
    ),
    hybrid: bool = typer.Option(
        True,
        "--hybrid/--no-hybrid",
        help="Fuse keyword and vector results when embeddings are enabled.",
    ),
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """Search Drive documents visible to the current user."""
    result = DriveV2Client().search(
        query=query,
        limit=limit,
        source_type=source_type,
        occurred_after=occurred_after,
        occurred_before=occurred_before,
        hybrid=hybrid,
    )
    _require_ok(result)
    if json_output:
        _print_json(result)
        return
    results = result.get("results") or []
    if not results:
        console.print(f"[yellow]No Drive documents found for: {query}[/yellow]")
        return
    _print_documents_table(f"Drive Search ({len(results)})", results)


@v2_app.command("list")
def v2_list(
    limit: int = typer.Option(10, "--limit", "-n", help="Max documents."),
    source_type: str | None = typer.Option(None, "--source-type", help=_V2_SOURCE_TYPE_HELP),
    occurred_after: str | None = typer.Option(
        None, "--after", help="Only documents modified on/after this time."
    ),
    occurred_before: str | None = typer.Option(
        None, "--before", help="Only documents modified before this time."
    ),
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """List recently modified Drive documents visible to the current user."""
    result = DriveV2Client().list_documents(
        limit=limit,
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
        console.print("[yellow]No Drive documents found.[/yellow]")
        return
    _print_documents_table(f"Drive Documents ({len(results)})", results)


@v2_app.command("read")
def v2_read(
    document_id: str = typer.Argument(..., help="Document ID returned by v2 search/list."),
    max_chars: int = typer.Option(0, "--max-chars", help="Maximum content chars; 0 means full."),
    json_output: bool = typer.Option(
        True,
        "--json/--table",
        help="Output JSON (default) or human-readable text.",
    ),
) -> None:
    """Read a Drive document returned by v2 search or list."""
    result = DriveV2Client().read_document(document_id=document_id, max_chars=max_chars)
    _require_ok(result)
    if json_output:
        _print_json(result)
        return
    console.print(f"[bold]{result.get('title') or result.get('document_id')}[/bold]")
    if result.get("url"):
        console.print(f"[dim]{result['url']}[/dim]")
    console.print(result.get("content") or "")
    if result.get("truncated"):
        console.print(
            f"[yellow]Truncated at {result.get('chars')} of {result.get('total_chars')} chars.[/yellow]"
        )


@v2_app.command("latest-date")
def v2_latest_date(
    source_type: str | None = typer.Option(None, "--source-type", help=_V2_SOURCE_TYPE_HELP),
) -> None:
    """Show the latest indexed Drive timestamp as JSON."""
    result = DriveV2Client().latest_date(source_type=source_type)
    _require_ok(result)
    _print_json(result)


if __name__ == "__main__":
    app()
