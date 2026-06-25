"""CLI for Firefox Profiler data extraction."""

from dotenv import load_dotenv

load_dotenv()

import json
import re

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(
    name="profslice",
    help="Firefox Profiler data extraction for LLM analysis. No browser required.",
)


@app.command("health")
def health():
    """Assert profslice connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {"resolved_url": client.resolve_url("https://profiler.firefox.com/public/health")}
        payload = {"ok": True, "tool": "profslice", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "profslice", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def parse_time_range(time_range: str | None) -> tuple[float, float] | None:
    """Parse a time range string like '1s-2s', '1000-2000', or '1s+500ms'."""
    if not time_range:
        return None

    def parse_time(s: str) -> float:
        """Parse a time string to milliseconds."""
        s = s.strip()
        if s.endswith("ms"):
            return float(s[:-2])
        elif s.endswith("s"):
            return float(s[:-1]) * 1000
        elif s.endswith("m"):
            return float(s[:-1]) * 60 * 1000
        else:
            return float(s)

    if "+" in time_range and "-" not in time_range.split("+")[1]:
        parts = time_range.split("+")
        start = parse_time(parts[0])
        duration = parse_time(parts[1])
        return (start, start + duration)

    if "-" in time_range:
        match = re.match(r"([^-]+)-(.+)", time_range)
        if match:
            start = parse_time(match.group(1))
            end = parse_time(match.group(2))
            return (start, end)

    raise typer.BadParameter(f"Invalid time range format: {time_range}")


def get_client():
    from .client import ProfilerClient

    return ProfilerClient()


def get_analyzer(profile: dict):
    from .profile import ProfileAnalyzer

    return ProfileAnalyzer(profile)


@app.command()
def fetch(
    url: str = typer.Argument(
        ..., help="URL (share.firefox.dev, profiler.firefox.com) or local path"
    ),
    output: str = typer.Option("profile.json.gz", "-o", "--output", help="Output file path"),
    no_save: bool = typer.Option(False, "--no-save", help="Don't save, just print metadata"),
):
    """Fetch a profile from a Firefox Profiler URL."""
    client = get_client()

    try:
        console.print(f"[dim]Fetching profile from {url}...[/]")
        profile = client.fetch_profile(url)

        meta = profile.get("meta", {})
        threads = profile.get("threads", [])
        total_samples = sum(t.get("samples", {}).get("length", 0) for t in threads)

        console.print(f"\n[bold]Profile: {meta.get('product', 'Unknown')}[/]")
        console.print(f"Threads: {len(threads)}")
        console.print(f"Samples: {total_samples}")
        console.print(f"Interval: {meta.get('interval', 0)}ms")

        if not no_save:
            client.save_profile(profile, output)
            console.print(f"\n[green]Saved to {output}[/]")

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)
    finally:
        client.close()


@app.command()
def threads(
    profile_path: str = typer.Argument(..., help="Profile file path (.json or .json.gz)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    jsonl: bool = typer.Option(False, "--jsonl", help="Output as JSONL (one thread per line)"),
):
    """List threads with summary statistics."""
    with get_client() as client:
        profile = client.fetch_profile(profile_path)
        analyzer = get_analyzer(profile)

        summaries = analyzer.get_thread_summaries()

        if jsonl:
            for s in summaries:
                print(json.dumps(s.to_dict(), separators=(",", ":")))
            return

        if json_output:
            print(json.dumps([s.to_dict() for s in summaries], indent=2))
            return

        table = Table(title="Threads")
        table.add_column("TID", style="cyan")
        table.add_column("Name", style="green", max_width=30)
        table.add_column("Main", style="dim")
        table.add_column("Samples", justify="right")
        table.add_column("Markers", justify="right")
        table.add_column("Duration (ms)", justify="right")
        table.add_column("CPU (ms)", justify="right")

        for s in sorted(summaries, key=lambda x: x.sample_count, reverse=True):
            cpu_ms = s.cpu_delta_total_ns / 1_000_000 if s.cpu_delta_total_ns else 0
            table.add_row(
                str(s.tid),
                s.name,
                "✓" if s.is_main_thread else "",
                str(s.sample_count),
                str(s.marker_count),
                f"{s.duration_ms:.1f}",
                f"{cpu_ms:.1f}" if cpu_ms > 0 else "-",
            )

        console.print(table)


@app.command()
def samples(
    profile_path: str = typer.Argument(..., help="Profile file path"),
    thread: str = typer.Option(None, "--thread", "-t", help="Thread TID or name pattern"),
    time_range: str = typer.Option(
        None, "--time-range", "-r", help="Time range (e.g., 1s-2s, 1000-2000)"
    ),
    depth: int = typer.Option(10, "--depth", "-d", help="Max stack depth"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max samples to output"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON array"),
):
    """Extract samples with resolved stacks."""
    with get_client() as client:
        profile = client.fetch_profile(profile_path)
        analyzer = get_analyzer(profile)

        target_thread = analyzer.find_thread(thread)
        if not target_thread:
            console.print(f"[red]Thread not found: {thread}[/]")
            raise typer.Exit(1)

        parsed_range = parse_time_range(time_range)
        samples_list = analyzer.get_samples(target_thread, time_range=parsed_range, max_depth=depth)

        if limit and len(samples_list) > limit:
            samples_list = samples_list[:limit]

        if json_output:
            print(json.dumps([s.to_dict() for s in samples_list], indent=2))
        else:
            for s in samples_list:
                print(json.dumps(s.to_dict(), separators=(",", ":")))


@app.command()
def markers(
    profile_path: str = typer.Argument(..., help="Profile file path"),
    thread: str = typer.Option(None, "--thread", "-t", help="Thread TID or name pattern"),
    marker_type: str = typer.Option(None, "--type", help="Filter by marker type/name"),
    category: str = typer.Option(None, "--category", "-c", help="Filter by category"),
    time_range: str = typer.Option(None, "--time-range", "-r", help="Time range"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max markers to output"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON array"),
):
    """Extract markers (events) from a profile."""
    with get_client() as client:
        profile = client.fetch_profile(profile_path)
        analyzer = get_analyzer(profile)

        target_thread = analyzer.find_thread(thread)
        if not target_thread:
            console.print(f"[red]Thread not found: {thread}[/]")
            raise typer.Exit(1)

        parsed_range = parse_time_range(time_range)
        markers_list = analyzer.get_markers(
            target_thread,
            time_range=parsed_range,
            marker_type=marker_type,
            category_filter=category,
        )

        if limit and len(markers_list) > limit:
            markers_list = markers_list[:limit]

        if json_output:
            print(json.dumps([m.to_dict() for m in markers_list], indent=2))
        else:
            for m in markers_list:
                print(json.dumps(m.to_dict(), separators=(",", ":")))


@app.command()
def hotspots(
    profile_path: str = typer.Argument(..., help="Profile file path"),
    thread: str = typer.Option(None, "--thread", "-t", help="Thread TID or name pattern"),
    time_range: str = typer.Option(None, "--time-range", "-r", help="Time range"),
    by: str = typer.Option("function", "--by", "-b", help="Group by: function, stack"),
    top: int = typer.Option(20, "--top", "-n", help="Number of results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    jsonl: bool = typer.Option(False, "--jsonl", help="Output as JSONL"),
):
    """Compute hot functions or stacks."""
    with get_client() as client:
        profile = client.fetch_profile(profile_path)
        analyzer = get_analyzer(profile)

        target_thread = analyzer.find_thread(thread)
        if not target_thread:
            console.print(f"[red]Thread not found: {thread}[/]")
            raise typer.Exit(1)

        parsed_range = parse_time_range(time_range)
        hotspots_list = analyzer.compute_hotspots(
            target_thread,
            time_range=parsed_range,
            by=by,
            top_n=top,
        )

        if jsonl:
            for h in hotspots_list:
                print(json.dumps(h.to_dict(), separators=(",", ":")))
            return

        if json_output:
            print(json.dumps([h.to_dict() for h in hotspots_list], indent=2))
            return

        table = Table(title=f"Hotspots (by {by})")
        table.add_column("#", style="dim", justify="right")
        table.add_column("Function" if by == "function" else "Stack", style="cyan", max_width=60)
        table.add_column("Self", justify="right", style="yellow")
        table.add_column("Self %", justify="right")
        table.add_column("Total", justify="right", style="green")
        table.add_column("Total %", justify="right")

        for i, h in enumerate(hotspots_list, 1):
            table.add_row(
                str(i),
                h.name[:60] + "..." if len(h.name) > 60 else h.name,
                str(h.self_samples),
                f"{h.self_pct:.1f}%",
                str(h.total_samples),
                f"{h.total_pct:.1f}%",
            )

        console.print(table)


@app.command()
def timeline(
    profile_path: str = typer.Argument(..., help="Profile file path"),
    thread: str = typer.Option(None, "--thread", "-t", help="Thread TID or name pattern"),
    time_range: str = typer.Option(None, "--time-range", "-r", help="Time range"),
    bucket: str = typer.Option("1s", "--bucket", "-b", help="Bucket size (e.g., 100ms, 1s)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Generate time-bucketed analysis."""
    with get_client() as client:
        profile = client.fetch_profile(profile_path)
        analyzer = get_analyzer(profile)

        target_thread = analyzer.find_thread(thread)
        if not target_thread:
            console.print(f"[red]Thread not found: {thread}[/]")
            raise typer.Exit(1)

        bucket_ms = 1000.0
        if bucket.endswith("ms"):
            bucket_ms = float(bucket[:-2])
        elif bucket.endswith("s"):
            bucket_ms = float(bucket[:-1]) * 1000
        elif bucket.endswith("m"):
            bucket_ms = float(bucket[:-1]) * 60 * 1000
        else:
            bucket_ms = float(bucket)

        if bucket_ms <= 0:
            console.print("[red]Bucket size must be positive[/]")
            raise typer.Exit(1)

        parsed_range = parse_time_range(time_range)
        buckets = analyzer.compute_timeline(
            target_thread, bucket_size_ms=bucket_ms, time_range=parsed_range
        )

        if json_output:
            print(json.dumps([b.to_dict() for b in buckets], indent=2))
            return

        for b in buckets:
            print(json.dumps(b.to_dict(), separators=(",", ":")))


@app.command()
def info(
    profile_path: str = typer.Argument(..., help="Profile file path"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Show profile metadata and summary."""
    with get_client() as client:
        profile = client.fetch_profile(profile_path)

        meta = profile.get("meta", {})
        threads = profile.get("threads", [])

        info_dict = {
            "product": meta.get("product", "Unknown"),
            "platform": meta.get("platform"),
            "interval_ms": meta.get("interval", 0),
            "version": meta.get("version", 0),
            "thread_count": len(threads),
            "total_samples": sum(t.get("samples", {}).get("length", 0) for t in threads),
            "total_markers": sum(t.get("markers", {}).get("length", 0) for t in threads),
            "categories": [c.get("name") for c in meta.get("categories", [])],
        }

        if json_output:
            print(json.dumps(info_dict, indent=2))
            return

        console.print(f"\n[bold cyan]{info_dict['product']}[/]")
        console.print(f"Platform: {info_dict['platform'] or 'Unknown'}")
        console.print(f"Interval: {info_dict['interval_ms']}ms")
        console.print(f"Threads: {info_dict['thread_count']}")
        console.print(f"Total samples: {info_dict['total_samples']}")
        console.print(f"Total markers: {info_dict['total_markers']}")
        console.print(f"Categories: {', '.join(info_dict['categories'][:10])}")


if __name__ == "__main__":
    app()
