"""CLI for Datadog observability."""

import json

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="datadog",
    help="Datadog CLI — logs, metrics, monitors, hosts, and dashboards (read-only)",
    no_args_is_help=True,
)


def get_client():
    from .client import DatadogClient

    return DatadogClient()


def _emit(result) -> None:
    print(json.dumps(result, indent=2, ensure_ascii=False, default=str))


@app.command("health")
def health():
    """Assert Datadog connectivity and auth with a safe read-only check."""
    client = get_client()
    try:
        details = client.validate()
        ok = bool(details.get("valid", details))
        payload = {"ok": ok, "tool": "datadog", "error": None, "details": details}
        if not ok:
            payload["error"] = "Datadog credentials did not validate"
    except Exception as exc:
        payload = {"ok": False, "tool": "datadog", "error": str(exc), "details": {}}
        _emit(payload)
        raise typer.Exit(1) from exc
    finally:
        client.close()
    _emit(payload)
    if not ok:
        raise typer.Exit(1)


@app.command("logs")
def search_logs(
    query: str = typer.Argument(..., help="Datadog logs query"),
    start: str = typer.Option("15m", "--start", "-s", help="Start time, e.g. 15m, 2h, ISO, epoch"),
    end: str = typer.Option(None, "--end", "-e", help="End time; defaults to now"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max logs, 1-1000"),
    sort: str = typer.Option("-timestamp", "--sort", help="timestamp or -timestamp"),
):
    """Search logs."""
    _emit(get_client().search_logs(query=query, start=start, end=end, limit=limit, sort=sort))


@app.command("metrics")
def query_metrics(
    query: str = typer.Argument(..., help="Datadog metric query, e.g. avg:system.cpu.user{*}"),
    start: str = typer.Option("1h", "--start", "-s", help="Start time, e.g. 1h, ISO, epoch"),
    end: str = typer.Option(None, "--end", "-e", help="End time; defaults to now"),
):
    """Query metrics."""
    _emit(get_client().query_metrics(query=query, start=start, end=end))


@app.command("monitors")
def list_monitors(
    query: str = typer.Option(None, "--query", "-q", help="Filter monitor name"),
    tags: str = typer.Option(None, "--tags", "-t", help="Monitor tags, comma-separated"),
    group_states: str = typer.Option(None, "--states", help="Group states, comma-separated"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max monitors"),
):
    """List monitors."""
    _emit(
        get_client().list_monitors(
            query=query,
            tags=tags,
            group_states=group_states,
            limit=limit,
        )
    )


@app.command("monitor-search")
def search_monitors(
    query: str = typer.Argument("", help="Datadog monitor search query"),
    page: int = typer.Option(0, "--page", help="Result page"),
    per_page: int = typer.Option(30, "--per-page", help="Results per page, 1-100"),
):
    """Search monitors with monitor search syntax."""
    _emit(get_client().search_monitors(query=query, page=page, per_page=per_page))


@app.command("monitor")
def get_monitor(
    monitor_id: int = typer.Argument(..., help="Monitor id"),
):
    """Get one monitor."""
    _emit(get_client().get_monitor(monitor_id=monitor_id))


@app.command("hosts")
def list_hosts(
    filter: str = typer.Option(None, "--filter", "-f", help="Host filter query"),
    sort_field: str = typer.Option("last_reported_time", "--sort-field", help="Sort field"),
    sort_dir: str = typer.Option("desc", "--sort-dir", help="asc or desc"),
    count: int = typer.Option(100, "--count", "-n", help="Max hosts"),
):
    """List hosts."""
    _emit(
        get_client().list_hosts(
            filter=filter,
            sort_field=sort_field,
            sort_dir=sort_dir,
            count=count,
        )
    )


@app.command("dashboards")
def search_dashboards(
    query: str = typer.Option(None, "--query", "-q", help="Dashboard search query"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max dashboards"),
):
    """Search dashboards."""
    _emit(get_client().search_dashboards(query=query, limit=limit))


@app.command("dashboard")
def get_dashboard(
    dashboard_id: str = typer.Argument(..., help="Dashboard id"),
):
    """Get one dashboard."""
    _emit(get_client().get_dashboard(dashboard_id=dashboard_id))


if __name__ == "__main__":
    app()
