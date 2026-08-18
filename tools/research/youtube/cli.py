"""CLI for YouTube Data API."""

import json
import re
from typing import Annotated

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(name="youtube", help="YouTube CLI for video data")


@app.command("health")
def health():
    """Assert youtube connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search("OpenAI", max_results=1)
        payload = {"ok": True, "tool": "youtube", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "youtube", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import YouTubeClient

    return YouTubeClient()


def format_number(value: int | None) -> str:
    """Format large numbers with B/M/K suffixes."""
    if value is None:
        return "N/A"
    if value >= 1e9:
        return f"{value / 1e9:.1f}B"
    elif value >= 1e6:
        return f"{value / 1e6:.1f}M"
    elif value >= 1e3:
        return f"{value / 1e3:.1f}K"
    return str(value)


def parse_duration(duration: str | None) -> str:
    """Parse ISO 8601 duration to human-readable format."""
    if not duration:
        return "N/A"
    match = re.match(r"PT(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?", duration)
    if not match:
        return duration
    hours, mins, secs = match.groups()
    parts = []
    if hours:
        parts.append(f"{hours}h")
    if mins:
        parts.append(f"{mins}m")
    if secs:
        parts.append(f"{secs}s")
    return " ".join(parts) if parts else "0s"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


def format_timestamp(seconds: float | None) -> str:
    """Format seconds as HH:MM:SS."""
    if seconds is None:
        return "N/A"
    total_seconds = max(0, int(seconds))
    hours, remainder = divmod(total_seconds, 3600)
    minutes, secs = divmod(remainder, 60)
    return f"{hours:02d}:{minutes:02d}:{secs:02d}"


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    type: str = typer.Option("video", "--type", "-t", help="Type: video, channel, playlist"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for videos, channels, or playlists."""
    client = get_client()
    data = client.search(query, max_results=limit, type=type)

    items = data.get("items", [])

    if json_output:
        print(json.dumps(items, indent=2))
        return

    if not items:
        console.print(f"[yellow]No results for '{query}'[/]")
        raise typer.Exit()

    if markdown:
        rows = []
        for item in items:
            snippet = item.get("snippet", {})
            id_info = item.get("id", {})
            video_id = (
                id_info.get("videoId") or id_info.get("channelId") or id_info.get("playlistId", "")
            )
            rows.append(
                [
                    snippet.get("title", "")[:50],
                    snippet.get("channelTitle", "")[:25],
                    video_id,
                ]
            )
        print_markdown_table(["Title", "Channel", "ID"], rows)
        return

    table = Table(title=f"Search Results: '{query}'")
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Channel", style="white", max_width=25)
    table.add_column("ID", style="dim")

    for item in items:
        snippet = item.get("snippet", {})
        id_info = item.get("id", {})
        video_id = (
            id_info.get("videoId") or id_info.get("channelId") or id_info.get("playlistId", "")
        )
        table.add_row(
            snippet.get("title", "")[:50],
            snippet.get("channelTitle", "")[:25],
            video_id,
        )

    console.print(table)


@app.command()
def video(
    video_id: str = typer.Argument(..., help="Video ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get video details."""
    client = get_client()
    data = client.get_video(video_id)

    items = data.get("items", [])
    if not items:
        console.print(f"[red]Video not found: {video_id}[/]")
        raise typer.Exit(1)

    video_data = items[0]

    if json_output:
        print(json.dumps(video_data, indent=2))
        return

    snippet = video_data.get("snippet", {})
    stats = video_data.get("statistics", {})
    content = video_data.get("contentDetails", {})

    views = int(stats.get("viewCount", 0)) if stats.get("viewCount") else None
    likes = int(stats.get("likeCount", 0)) if stats.get("likeCount") else None
    comments = int(stats.get("commentCount", 0)) if stats.get("commentCount") else None
    duration = parse_duration(content.get("duration"))

    if markdown:
        print(f"# {snippet.get('title', 'Unknown')}\n")
        print(f"**Channel:** {snippet.get('channelTitle', 'N/A')}")
        print(f"**Duration:** {duration}")
        print(f"**Views:** {format_number(views)}")
        print(f"**Likes:** {format_number(likes)}")
        print(f"**Comments:** {format_number(comments)}")
        print(f"**Published:** {snippet.get('publishedAt', 'N/A')}")
        print(f"\n**URL:** https://www.youtube.com/watch?v={video_id}")
        if snippet.get("description"):
            print(f"\n**Description:**\n{snippet.get('description')[:500]}")
        return

    console.print(f"\n[bold cyan]{snippet.get('title', 'Unknown')}[/]")
    console.print(f"[dim]Channel: {snippet.get('channelTitle', 'N/A')}[/]\n")
    console.print(f"Duration: [yellow]{duration}[/]")
    console.print(f"Views: [green]{format_number(views)}[/]")
    console.print(f"Likes: [blue]{format_number(likes)}[/]")
    console.print(f"Comments: {format_number(comments)}")
    console.print(f"Published: {snippet.get('publishedAt', 'N/A')}")
    console.print(f"\nURL: https://www.youtube.com/watch?v={video_id}")
    if snippet.get("description"):
        console.print(f"\n[dim]{snippet.get('description')[:500]}[/]")


@app.command()
def channel(
    channel_id: str = typer.Argument(..., help="Channel ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get channel details."""
    client = get_client()
    data = client.get_channel(channel_id)

    items = data.get("items", [])
    if not items:
        console.print(f"[red]Channel not found: {channel_id}[/]")
        raise typer.Exit(1)

    channel_data = items[0]

    if json_output:
        print(json.dumps(channel_data, indent=2))
        return

    snippet = channel_data.get("snippet", {})
    stats = channel_data.get("statistics", {})

    subs = int(stats.get("subscriberCount", 0)) if stats.get("subscriberCount") else None
    views = int(stats.get("viewCount", 0)) if stats.get("viewCount") else None
    videos = int(stats.get("videoCount", 0)) if stats.get("videoCount") else None

    if markdown:
        print(f"# {snippet.get('title', 'Unknown')}\n")
        print(f"**Subscribers:** {format_number(subs)}")
        print(f"**Total Views:** {format_number(views)}")
        print(f"**Videos:** {format_number(videos)}")
        print(f"**Created:** {snippet.get('publishedAt', 'N/A')}")
        print(f"\n**URL:** https://www.youtube.com/channel/{channel_id}")
        if snippet.get("description"):
            print(f"\n**Description:**\n{snippet.get('description')[:500]}")
        return

    console.print(f"\n[bold cyan]{snippet.get('title', 'Unknown')}[/]")
    console.print(f"[dim]@{snippet.get('customUrl', channel_id)}[/]\n")
    console.print(f"Subscribers: [green]{format_number(subs)}[/]")
    console.print(f"Total Views: [blue]{format_number(views)}[/]")
    console.print(f"Videos: [yellow]{format_number(videos)}[/]")
    console.print(f"Created: {snippet.get('publishedAt', 'N/A')}")
    console.print(f"\nURL: https://www.youtube.com/channel/{channel_id}")
    if snippet.get("description"):
        console.print(f"\n[dim]{snippet.get('description')[:500]}[/]")


@app.command("transcripts")
def transcripts(
    video_id: str = typer.Argument(..., help="YouTube video ID or URL"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """List the public caption tracks for a video."""
    client = get_client()
    data = client.list_transcripts(video_id)
    tracks = data.get("tracks", [])

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        rows = [
            [
                track.get("language", ""),
                track.get("language_code", ""),
                "auto" if track.get("is_generated") else "manual",
            ]
            for track in tracks
        ]
        print_markdown_table(["Language", "Code", "Type"], rows)
        return

    table = Table(title=f"Caption Tracks: {data.get('video_id', video_id)}")
    table.add_column("Language", style="cyan")
    table.add_column("Code", style="white")
    table.add_column("Type", style="dim")
    for track in tracks:
        table.add_row(
            track.get("language", ""),
            track.get("language_code", ""),
            "auto-generated" if track.get("is_generated") else "manual",
        )
    console.print(table)


@app.command()
def transcript(
    video_id: str = typer.Argument(..., help="YouTube video ID or URL"),
    language: Annotated[
        list[str] | None,
        typer.Option(
            "--language",
            "-l",
            help="Preferred caption language code. Repeat the flag for fallbacks.",
        ),
    ] = None,
    start: Annotated[
        str | None,
        typer.Option(
            "--start",
            help="Start offset in seconds or HH:MM:SS. Negative offsets are relative to the end.",
        ),
    ] = None,
    end: Annotated[
        str | None,
        typer.Option(
            "--end",
            help="End offset in seconds or HH:MM:SS. Negative offsets are relative to the end.",
        ),
    ] = None,
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Download a timestamped public transcript for a video."""
    client = get_client()
    data = client.get_transcript(
        video_id,
        language_codes=language,
        start_time=start,
        end_time=end,
    )

    transcript_rows = data.get("transcript", [])
    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        print(
            f"# Transcript ({data.get('language_code', 'unknown')}, "
            f"{'auto-generated' if data.get('is_generated') else 'manual'})\n"
        )
        for row in transcript_rows:
            print(f"- [{format_timestamp(row.get('start'))}] {row.get('text', '')}")
        return

    console.print(
        f"\n[bold cyan]Transcript[/] {data.get('language', 'Unknown')} "
        f"({'auto-generated' if data.get('is_generated') else 'manual'})"
    )
    console.print(
        f"[dim]Window: {format_timestamp(data.get('window_start'))} → "
        f"{format_timestamp(data.get('window_end'))}[/]"
    )
    for row in transcript_rows:
        console.print(f"[cyan][{format_timestamp(row.get('start'))}][/cyan] {row.get('text', '')}")


if __name__ == "__main__":
    app()
