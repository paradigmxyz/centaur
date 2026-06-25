"""CLI for the Amplitude Dashboard REST + Taxonomy APIs."""

import json

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="amplitude", help="Amplitude product analytics (Dashboard REST + Taxonomy)")


@app.command("health")
def health():
    """Assert amplitude connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.events_list()
        payload = {"ok": True, "tool": "amplitude", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "amplitude", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import AmplitudeClient

    return AmplitudeClient()


def _run(fn):
    """Call a client method, printing errors and exiting non-zero on failure."""
    try:
        return fn()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


def _dump(result: dict) -> None:
    print(json.dumps(result, indent=2))


@app.command()
def segmentation(
    event: str = typer.Argument(..., help="Event type (use '_all' for any active event)"),
    start: str = typer.Option(..., "--start", "-s", help="Start date (YYYY-MM-DD or YYYYMMDD)"),
    end: str = typer.Option(..., "--end", "-e", help="End date (YYYY-MM-DD or YYYYMMDD)"),
    metric: str = typer.Option("totals", "--metric", "-m", help="totals, uniques, average, ..."),
    interval: int = typer.Option(1, "--interval", "-i", help="1=daily, 7=weekly, 30=monthly"),
    group_by: str = typer.Option(None, "--group-by", "-g", help="Property to break down by"),
    group_by_type: str = typer.Option("event", "--group-by-type", help="'event' or 'user'"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max breakdown groups"),
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON"),
):
    """Event segmentation — counts/uniques of an event over time."""
    client = get_client()
    result = _run(
        lambda: client.segmentation(
            event=event,
            start=start,
            end=end,
            metric=metric,
            interval=interval,
            group_by=group_by,
            group_by_type=group_by_type,
            limit=limit,
        )
    )

    if json_output:
        _dump(result)
        return

    data = result.get("data", {})
    x_values = data.get("xValues", [])
    series = data.get("series", [])
    labels = data.get("seriesLabels") or data.get("seriesMeta") or []

    if not x_values or not series:
        _dump(result)
        return

    table = Table(title=f"{event} — {metric} ({start} → {end})")
    table.add_column("Date", style="dim")
    for idx in range(len(series)):
        label = labels[idx] if idx < len(labels) else f"series {idx + 1}"
        table.add_column(str(label), style="cyan", justify="right")

    for row_idx, x in enumerate(x_values):
        cells = [str(s[row_idx]) if row_idx < len(s) else "" for s in series]
        table.add_row(str(x), *cells)

    console.print(table)


@app.command()
def funnel(
    events: list[str] = typer.Argument(..., help="Event types in step order"),
    start: str = typer.Option(..., "--start", "-s", help="Start date (YYYY-MM-DD or YYYYMMDD)"),
    end: str = typer.Option(..., "--end", "-e", help="End date (YYYY-MM-DD or YYYYMMDD)"),
    mode: str = typer.Option("ordered", "--mode", help="ordered, unordered, or sequential"),
    window_days: int = typer.Option(None, "--window-days", help="Conversion window in days"),
):
    """Funnel conversion across an ordered sequence of events."""
    client = get_client()
    result = _run(
        lambda: client.funnel(
            events=events, start=start, end=end, mode=mode, conversion_window_days=window_days
        )
    )
    _dump(result)


@app.command()
def retention(
    start_event: str = typer.Argument(..., help="Event that starts the measurement"),
    return_event: str = typer.Argument("_all", help="Return event ('_all' for any)"),
    start: str = typer.Option(..., "--start", "-s", help="Start date (YYYY-MM-DD or YYYYMMDD)"),
    end: str = typer.Option(..., "--end", "-e", help="End date (YYYY-MM-DD or YYYYMMDD)"),
    mode: str = typer.Option("n-day", "--mode", help="n-day, rolling, or bracket"),
    interval: int = typer.Option(1, "--interval", "-i", help="1=daily, 7=weekly, 30=monthly"),
):
    """Retention analysis between a start event and a return event."""
    client = get_client()
    result = _run(
        lambda: client.retention(
            start_event=start_event,
            return_event=return_event,
            start=start,
            end=end,
            retention_mode=mode,
            interval=interval,
        )
    )
    _dump(result)


@app.command("events-list")
def events_list(
    json_output: bool = typer.Option(False, "--json", help="Output raw JSON"),
):
    """List every event type defined in the project."""
    client = get_client()
    result = _run(client.events_list)

    if json_output:
        _dump(result)
        return

    rows = result.get("data", result) if isinstance(result, dict) else result
    if not isinstance(rows, list) or not rows:
        _dump(result)
        return

    table = Table(title="Event Types")
    table.add_column("Event", style="cyan")
    table.add_column("Display Name", style="green")
    table.add_column("Non-active", style="dim", justify="center")
    for row in rows:
        table.add_row(
            str(row.get("name", "")),
            str(row.get("display", row.get("name", ""))),
            "yes" if row.get("non_active") else "",
        )
    console.print(table)


@app.command("user-activity")
def user_activity(
    user: str = typer.Argument(..., help="Amplitude ID"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max events (up to 1000)"),
    offset: int = typer.Option(0, "--offset", help="Pagination offset"),
    direction: str = typer.Option("latest", "--direction", help="'latest' or 'earliest'"),
):
    """Fetch a single user's event stream by Amplitude ID."""
    client = get_client()
    result = _run(
        lambda: client.user_activity(user=user, limit=limit, offset=offset, direction=direction)
    )
    _dump(result)


@app.command("user-search")
def user_search(
    user: str = typer.Argument(..., help="Amplitude ID, Device ID, User ID, or prefix"),
):
    """Search for a user by identifier or prefix."""
    client = get_client()
    result = _run(lambda: client.user_search(user))
    _dump(result)


@app.command()
def realtime(
    interval: int = typer.Option(None, "--interval", "-i", help="Bucket size in seconds"),
):
    """Real-time active user counts."""
    client = get_client()
    result = _run(lambda: client.realtime(interval=interval))
    _dump(result)


@app.command()
def annotations():
    """List chart annotations for the project."""
    client = get_client()
    result = _run(client.annotations)
    _dump(result)


@app.command("taxonomy-events")
def taxonomy_events():
    """List event types in the tracking plan (Taxonomy API)."""
    client = get_client()
    result = _run(client.taxonomy_events)
    _dump(result)


@app.command("taxonomy-event-properties")
def taxonomy_event_properties(
    event_type: str = typer.Argument(..., help="Event type whose properties to list"),
):
    """List event properties for an event type (Taxonomy API)."""
    client = get_client()
    result = _run(lambda: client.taxonomy_event_properties(event_type))
    _dump(result)


@app.command("taxonomy-user-properties")
def taxonomy_user_properties():
    """List user properties in the tracking plan (Taxonomy API)."""
    client = get_client()
    result = _run(client.taxonomy_user_properties)
    _dump(result)


if __name__ == "__main__":
    app()
