"""CLI for Claap recordings and transcripts."""

from __future__ import annotations

from dotenv import load_dotenv

load_dotenv()

import json
import sys

import typer
from centaur_sdk import Table
from rich.console import Console
from rich.markdown import Markdown

from .client import ClaapClient, TranscriptFormat

app = typer.Typer(name="claap", help="Query Claap recordings and transcripts")
console = Console()


def _client() -> ClaapClient:
    return ClaapClient()


def _recording_rows(payload: dict) -> list[dict]:
    result = payload.get("result", payload)
    recordings = result.get("recordings") if isinstance(result, dict) else None
    return recordings if isinstance(recordings, list) else []


def _recording(payload: dict) -> dict:
    result = payload.get("result", payload)
    recording = result.get("recording") if isinstance(result, dict) else None
    return recording if isinstance(recording, dict) else result


@app.command("list")
def list_recordings(
    limit: int = typer.Option(20, "--limit", "-n", help="Max recordings to return"),
    created_after: str | None = typer.Option(
        None, "--created-after", help="YYYY-MM-DD or ISO"
    ),
    created_before: str | None = typer.Option(
        None, "--created-before", help="YYYY-MM-DD or ISO"
    ),
    channel_id: str | None = typer.Option(
        None, "--channel-id", help="Claap channel/folder id"
    ),
    recorder_email: str | None = typer.Option(
        None, "--recorder-email", help="Recorder email"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON"),
) -> None:
    """List Claap recordings."""
    with _client() as client:
        payload = client.list_recordings(
            limit=limit,
            created_after=created_after,
            created_before=created_before,
            channel_id=channel_id,
            recorder_email=recorder_email,
        )

    if json_output:
        print(json.dumps(payload, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    recordings = _recording_rows(payload)
    if not recordings:
        console.print("[yellow]No recordings found.[/]")
        raise typer.Exit()

    table = Table(title=f"Claap Recordings ({len(recordings)})")
    table.add_column("ID", style="dim", max_width=18)
    table.add_column("Title", style="cyan", max_width=52)
    table.add_column("State", style="green", max_width=12)
    table.add_column("Created", style="white", max_width=20)
    table.add_column("Recorder", style="magenta", max_width=28)

    for item in recordings:
        recorder = item.get("recorder") or {}
        recorder_name = recorder.get("name") or recorder.get("email") or ""
        table.add_row(
            str(item.get("id", "")),
            str(item.get("title") or "Untitled"),
            str(item.get("state") or ""),
            str(item.get("createdAt") or ""),
            str(recorder_name),
        )
    console.print(table)


@app.command("get")
def get_recording(
    recording_id_or_url: str = typer.Argument(..., help="Claap recording id or URL"),
    no_ai_fields: bool = typer.Option(
        False, "--no-ai-fields", help="Do not request AI fields"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON"),
) -> None:
    """Get one Claap recording."""
    with _client() as client:
        payload = client.get_recording(
            recording_id_or_url,
            return_ai_fields=not no_ai_fields,
        )

    if json_output:
        print(json.dumps(payload, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    recording = _recording(payload)
    title = recording.get("title") or "Untitled Claap recording"
    console.print(f"[bold cyan]{title}[/]")
    for label, key in [
        ("ID", "id"),
        ("URL", "url"),
        ("State", "state"),
        ("Created", "createdAt"),
        ("Duration", "durationSeconds"),
    ]:
        value = recording.get(key)
        if value is not None:
            console.print(f"[bold]{label}:[/] {value}")

    outlines = recording.get("outlines") or []
    if outlines:
        text = "\n".join(
            item.get("text", "") for item in outlines if isinstance(item, dict)
        ).strip()
        if text:
            console.print()
            console.print(Markdown(text))


@app.command("transcript")
def transcript(
    recording_id_or_url: str = typer.Argument(..., help="Claap recording id or URL"),
    format: TranscriptFormat = typer.Option("json", "--format", help="json or text"),
    lang: str | None = typer.Option(None, "--lang", help="Transcript language"),
    json_output: bool = typer.Option(False, "--json", help="Output JSON wrapper"),
) -> None:
    """Get a Claap recording transcript."""
    with _client() as client:
        payload = client.get_transcript(recording_id_or_url, format=format, lang=lang)

    if json_output or format == "json":
        print(json.dumps(payload, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    print(payload.get("transcript", ""), file=sys.stdout)


@app.command("bundle")
def bundle(
    recording_id_or_url: str = typer.Argument(..., help="Claap recording id or URL"),
    transcript_format: TranscriptFormat = typer.Option(
        "json", "--transcript-format", help="json or text"
    ),
    lang: str | None = typer.Option(None, "--lang", help="Transcript language"),
) -> None:
    """Get recording metadata and transcript in one JSON payload."""
    with _client() as client:
        payload = client.get_recording_bundle(
            recording_id_or_url,
            transcript_format=transcript_format,
            lang=lang,
        )
    print(json.dumps(payload, indent=2, ensure_ascii=False), file=sys.stdout)


if __name__ == "__main__":
    app()
