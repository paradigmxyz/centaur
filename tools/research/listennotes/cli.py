"""CLI for Listen Notes API."""

from dotenv import load_dotenv

load_dotenv()

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="listennotes", help="Listen Notes CLI for podcast data")


@app.command("health")
def health():
    """Assert listennotes connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.search("technology", type="podcast")
        payload = {"ok": True, "tool": "listennotes", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "listennotes", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import ListenNotesClient

    return ListenNotesClient()


def format_duration(seconds: int | None) -> str:
    """Format duration in seconds to human-readable string."""
    if seconds is None:
        return "N/A"
    mins = seconds // 60
    if mins >= 60:
        hours = mins // 60
        mins = mins % 60
        return f"{hours}h {mins}m"
    return f"{mins}m"


def print_markdown_table(headers: list[str], rows: list[list[str]]) -> None:
    """Print a markdown-formatted table."""
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    type: str = typer.Option("episode", "--type", "-t", help="Search type: episode or podcast"),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown table"),
):
    """Search for episodes or podcasts."""
    client = get_client()
    data = client.search(query, type=type)

    results = data.get("results", [])[:limit]

    if json_output:
        print(json.dumps(results, indent=2))
        return

    if not results:
        console.print(f"[yellow]No results for '{query}'[/]")
        raise typer.Exit()

    if type == "episode":
        if markdown:
            rows = []
            for ep in results:
                rows.append(
                    [
                        ep.get("title_original", "")[:50],
                        ep.get("podcast", {}).get("title_original", "")[:30],
                        format_duration(ep.get("audio_length_sec")),
                        ep.get("id", ""),
                    ]
                )
            print_markdown_table(["Title", "Podcast", "Duration", "ID"], rows)
            return

        table = Table(title=f"Episode Search: '{query}'")
        table.add_column("Title", style="cyan", max_width=50)
        table.add_column("Podcast", style="white", max_width=30)
        table.add_column("Duration", style="yellow", justify="right")
        table.add_column("ID", style="dim")

        for ep in results:
            table.add_row(
                ep.get("title_original", "")[:50],
                ep.get("podcast", {}).get("title_original", "")[:30],
                format_duration(ep.get("audio_length_sec")),
                ep.get("id", ""),
            )

        console.print(table)
    else:
        if markdown:
            rows = []
            for pod in results:
                rows.append(
                    [
                        pod.get("title_original", "")[:40],
                        pod.get("publisher_original", "")[:30],
                        str(pod.get("total_episodes", "N/A")),
                        pod.get("id", ""),
                    ]
                )
            print_markdown_table(["Title", "Publisher", "Episodes", "ID"], rows)
            return

        table = Table(title=f"Podcast Search: '{query}'")
        table.add_column("Title", style="cyan", max_width=40)
        table.add_column("Publisher", style="white", max_width=30)
        table.add_column("Episodes", style="yellow", justify="right")
        table.add_column("ID", style="dim")

        for pod in results:
            table.add_row(
                pod.get("title_original", "")[:40],
                pod.get("publisher_original", "")[:30],
                str(pod.get("total_episodes", "N/A")),
                pod.get("id", ""),
            )

        console.print(table)


@app.command()
def podcast(
    podcast_id: str = typer.Argument(..., help="Podcast ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get podcast details."""
    client = get_client()
    data = client.get_podcast(podcast_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        print(f"# {data.get('title', 'Unknown')}\n")
        print(f"**Publisher:** {data.get('publisher', 'N/A')}")
        print(f"**Episodes:** {data.get('total_episodes', 'N/A')}")
        print(f"**Language:** {data.get('language', 'N/A')}")
        print(f"**Country:** {data.get('country', 'N/A')}")
        if data.get("description"):
            print(f"\n**Description:**\n{data.get('description')[:500]}")
        return

    console.print(f"\n[bold cyan]{data.get('title', 'Unknown')}[/]")
    console.print(f"[dim]Publisher: {data.get('publisher', 'N/A')}[/]\n")
    console.print(f"Episodes: [yellow]{data.get('total_episodes', 'N/A')}[/]")
    console.print(f"Language: {data.get('language', 'N/A')}")
    console.print(f"Country: {data.get('country', 'N/A')}")
    if data.get("description"):
        console.print(f"\n[dim]{data.get('description')[:500]}[/]")


@app.command()
def episode(
    episode_id: str = typer.Argument(..., help="Episode ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get episode details."""
    client = get_client()
    data = client.get_episode(episode_id)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    duration = format_duration(data.get("audio_length_sec"))

    if markdown:
        print(f"# {data.get('title', 'Unknown')}\n")
        print(f"**Podcast:** {data.get('podcast', {}).get('title', 'N/A')}")
        print(f"**Duration:** {duration}")
        print(f"**Published:** {data.get('pub_date_ms', 'N/A')}")
        if data.get("audio"):
            print(f"**Audio:** {data.get('audio')}")
        if data.get("description"):
            print(f"\n**Description:**\n{data.get('description')[:500]}")
        return

    console.print(f"\n[bold cyan]{data.get('title', 'Unknown')}[/]")
    console.print(f"[dim]Podcast: {data.get('podcast', {}).get('title', 'N/A')}[/]\n")
    console.print(f"Duration: [yellow]{duration}[/]")
    if data.get("audio"):
        console.print(f"Audio: {data.get('audio')}")
    if data.get("description"):
        console.print(f"\n[dim]{data.get('description')[:500]}[/]")


if __name__ == "__main__":
    app()
