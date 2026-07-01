"""Granola CLI for AI agents."""

from datetime import datetime

from dotenv import load_dotenv

load_dotenv()

import json
import typer
from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel
from rich.table import Table

app = typer.Typer(name="granola", help="Query Granola meeting notes and transcripts")


@app.command("health")
def health():
    """Assert granola connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_notes(page_size=1)
        payload = {"ok": True, "tool": "granola", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "granola", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def _format_date(date_str: str | None) -> str:
    """Format ISO date string to readable format."""
    if not date_str:
        return "-"
    try:
        dt = datetime.fromisoformat(date_str.replace("Z", "+00:00"))
        return dt.strftime("%Y-%m-%d %H:%M")
    except (ValueError, AttributeError):
        return date_str[:16] if date_str else "-"


@app.command("list")
def list_notes(
    limit: int = typer.Option(20, "--limit", "-n", help="Max notes to return"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full titles"),
    after: str | None = typer.Option(None, "--after", help="Created after (ISO date)"),
):
    """List recent meeting notes across the workspace."""
    from .client import GranolaClient

    client = GranolaClient()
    notes = client.list_all_notes(limit=limit, created_after=after)

    if not notes:
        console.print("[yellow]No meeting notes found.[/yellow]")
        return

    table = Table(title=f"Granola Notes ({len(notes)})")
    table.add_column("ID", style="dim", max_width=20)
    table.add_column("Owner", style="magenta", max_width=20)
    table.add_column("Title", style="cyan", max_width=None if full else 50)
    table.add_column("Date", style="green")

    for note in notes:
        note_id = note.get("id", "")
        owner = note.get("owner", {})
        owner_name = owner.get("name") or owner.get("email", "")
        title = note.get("title") or "Untitled"
        if not full and len(title) > 47:
            title = title[:47] + "..."
        created = _format_date(note.get("created_at"))
        table.add_row(note_id, owner_name, title, created)

    console.print(table)


@app.command("get")
def get_note(
    note_id: str = typer.Argument(..., help="Note ID (e.g. not_xxxxxxxxxxxxx)"),
    raw: bool = typer.Option(False, "--raw", "-r", help="Output raw markdown"),
    transcript: bool = typer.Option(False, "--transcript", "-t", help="Include transcript"),
):
    """Get a specific meeting note by ID."""
    from .client import GranolaClient

    client = GranolaClient()
    note = client.get_note(note_id, include_transcript=transcript)

    title = note.get("title") or "Untitled"
    created = _format_date(note.get("created_at"))
    owner = note.get("owner", {})
    owner_name = owner.get("name") or owner.get("email", "")
    attendees = note.get("attendees", [])
    attendee_names = ", ".join(a.get("name") or a.get("email", "") for a in attendees)
    summary = note.get("summary_markdown") or note.get("summary_text") or ""

    if raw:
        print(f"# {title}\n")
        print(f"*{created} — {owner_name}*\n")
        if attendee_names:
            print(f"**Attendees:** {attendee_names}\n")
        print(summary)
    else:
        header = f"[bold]{title}[/bold]\n[dim]{created} — {owner_name}[/dim]"
        if attendee_names:
            header += f"\n[dim]Attendees: {attendee_names}[/dim]"
        console.print(Panel(header))
        if summary:
            console.print(Markdown(summary))
        else:
            console.print("[yellow]No summary available.[/yellow]")

    if transcript and note.get("transcript"):
        console.print("\n[bold]Transcript:[/bold]")
        for utt in note["transcript"]:
            source = utt.get("speaker", {}).get("source", "unknown")
            text = utt.get("text", "")
            console.print(f"[bold cyan][{source}]:[/bold cyan] {text}")


@app.command("transcript")
def get_transcript(
    note_id: str = typer.Argument(..., help="Note ID"),
):
    """Get the transcript for a meeting note."""
    from .client import GranolaClient

    client = GranolaClient()
    utterances = client.get_transcript(note_id)

    if not utterances:
        console.print("[yellow]No transcript available.[/yellow]")
        return

    for utt in utterances:
        source = utt.get("speaker", {}).get("source", "unknown")
        text = utt.get("text", "")
        console.print(f"[bold cyan][{source}]:[/bold cyan] {text}")


@app.command("search")
def search_notes(
    query: str = typer.Argument(..., help="Search query (title match)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
):
    """Search meeting notes by title."""
    from .client import GranolaClient

    client = GranolaClient()
    notes = client.list_all_notes(limit=100)

    query_lower = query.lower()
    matches = [n for n in notes if query_lower in (n.get("title") or "").lower()][:limit]

    if not matches:
        console.print(f"[yellow]No notes matching: {query}[/yellow]")
        return

    table = Table(title=f"Search: '{query}' ({len(matches)} results)")
    table.add_column("ID", style="dim", max_width=20)
    table.add_column("Owner", style="magenta", max_width=20)
    table.add_column("Title", style="cyan")
    table.add_column("Date", style="green")

    for note in matches:
        note_id = note.get("id", "")
        owner = note.get("owner", {})
        owner_name = owner.get("name") or owner.get("email", "")
        title = note.get("title") or "Untitled"
        created = _format_date(note.get("created_at"))
        table.add_row(note_id, owner_name, title, created)

    console.print(table)


if __name__ == "__main__":
    app()
