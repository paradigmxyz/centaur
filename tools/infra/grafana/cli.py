"""CLI for Grafana API."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from centaur_sdk import Table

load_dotenv()

app = typer.Typer(name="grafana", help="Grafana CLI for dashboards, metrics, logs, and alerts")
console = Console()


def get_client():
    from .client import GrafanaClient

    return GrafanaClient()


@app.command("search")
def search_dashboards(
    query: str = typer.Argument(None, help="Search string"),
    tag: str = typer.Option(None, "--tag", "-t", help="Filter by tag"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search dashboards."""
    client = get_client()
    results = client.search_dashboards(query=query, tag=tag, limit=limit)

    if json_output:
        print(json.dumps(results, indent=2))
        return

    if not results:
        console.print("[yellow]No dashboards found[/]")
        return

    table = Table(title="Dashboards")
    table.add_column("UID", style="cyan")
    table.add_column("Title", style="white")
    table.add_column("Type", style="dim")
    table.add_column("Tags", style="green")

    for d in results:
        table.add_row(
            str(d.get("uid", "")),
            str(d.get("title", "")),
            str(d.get("type", "")),
            ", ".join(d.get("tags", [])),
        )

    console.print(table)


@app.command("get")
def get_dashboard(
    uid: str = typer.Argument(..., help="Dashboard UID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get a dashboard by UID."""
    client = get_client()
    result = client.get_dashboard(uid)

    if json_output:
        print(json.dumps(result, indent=2))
        return

    meta = result.get("meta", {})
    dash = result.get("dashboard", {})
    console.print(f"[bold]{dash.get('title', 'Untitled')}[/]")
    console.print(f"  UID: {dash.get('uid', '')}")
    console.print(f"  URL: {meta.get('url', '')}")
    console.print(f"  Version: {dash.get('version', '')}")
    panels = dash.get("panels", [])
    if panels:
        console.print(f"  Panels ({len(panels)}):")
        for p in panels:
            console.print(f"    - {p.get('title', '(untitled)')} [{p.get('type', '')}]")


@app.command("datasources")
def list_datasources(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List configured datasources."""
    client = get_client()
    results = client.list_datasources()

    if json_output:
        print(json.dumps(results, indent=2))
        return

    table = Table(title="Datasources")
    table.add_column("UID", style="cyan")
    table.add_column("Name", style="white")
    table.add_column("Type", style="green")
    table.add_column("URL", style="dim")

    for ds in results:
        table.add_row(
            str(ds.get("uid", "")),
            str(ds.get("name", "")),
            str(ds.get("type", "")),
            str(ds.get("url", "")),
        )

    console.print(table)


@app.command("query")
def query_metrics(
    expr: str = typer.Argument(..., help="MetricsQL expression"),
    datasource: str = typer.Option("victoriametrics", "--ds", help="Datasource UID"),
    start: str = typer.Option(None, "--start", "-s", help="Range start (RFC3339 / epoch)"),
    end: str = typer.Option(None, "--end", "-e", help="Range end"),
    step: str = typer.Option("60s", "--step", help="Range query step"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Run a MetricsQL query via datasource proxy."""
    client = get_client()
    result = client.query_metrics(
        expr=expr, datasource_uid=datasource, start=start, end=end, step=step
    )

    if json_output:
        print(json.dumps(result, indent=2))
        return

    data = result.get("data", {})
    results = data.get("result", [])

    if not results:
        console.print("[yellow]No results[/]")
        return

    for r in results:
        metric = r.get("metric", {})
        label = ", ".join(f"{k}={v}" for k, v in metric.items()) or "(scalar)"
        values = r.get("values") or [r.get("value", [])]
        console.print(f"[cyan]{label}[/]")
        for v in values[-5:]:
            if isinstance(v, list) and len(v) == 2:
                console.print(f"  {v[0]} → {v[1]}")


@app.command("vlogs")
def query_victorialogs(
    query: str = typer.Argument(..., help="LogsQL expression"),
    datasource: str = typer.Option("victorialogs", "--ds", help="Datasource UID"),
    start: str = typer.Option(None, "--start", "-s", help="Range start"),
    end: str = typer.Option(None, "--end", "-e", help="Range end"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max log lines"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Run a LogsQL query via VictoriaLogs datasource proxy."""
    client = get_client()
    results = client.query_victorialogs(
        query=query, datasource_uid=datasource, start=start, end=end, limit=limit
    )

    if json_output:
        print(json.dumps(results, indent=2))
        return

    if not results:
        console.print("[yellow]No results[/]")
        return

    for entry in results:
        msg = entry.get("_msg", "")
        stream = entry.get("_stream", "")
        time = entry.get("_time", "")
        console.print(f"[dim]{time}[/] [cyan]{stream}[/] {msg}")


@app.command("alerts")
def get_alerts(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List active alerts."""
    client = get_client()
    alerts = client.get_alerts()

    if json_output:
        print(json.dumps(alerts, indent=2))
        return

    if not alerts:
        console.print("[green]No active alerts[/]")
        return

    table = Table(title="Active Alerts")
    table.add_column("Name", style="red")
    table.add_column("State", style="yellow")
    table.add_column("Summary", style="white", max_width=60)

    for a in alerts:
        labels = a.get("labels", {})
        annotations = a.get("annotations", {})
        table.add_row(
            labels.get("alertname", ""),
            a.get("state", ""),
            annotations.get("summary", annotations.get("description", ""))[:60],
        )

    console.print(table)


@app.command("annotations")
def list_annotations(
    dashboard_uid: str = typer.Option(None, "--dashboard", "-d", help="Dashboard UID"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List annotations."""
    client = get_client()
    results = client.list_annotations(dashboard_uid=dashboard_uid, limit=limit)

    if json_output:
        print(json.dumps(results, indent=2))
        return

    if not results:
        console.print("[yellow]No annotations found[/]")
        return

    table = Table(title="Annotations")
    table.add_column("ID", style="dim")
    table.add_column("Text", style="white", max_width=60)
    table.add_column("Dashboard", style="cyan")
    table.add_column("Time", style="dim")

    for a in results:
        table.add_row(
            str(a.get("id", "")),
            str(a.get("text", ""))[:60],
            str(a.get("dashboardUID", "")),
            str(a.get("time", "")),
        )

    console.print(table)


@app.command("thread-debug-url")
def thread_debug_url(
    thread: str = typer.Argument(..., help="Slack thread URL, slack:C:ts key, or C:ts key"),
    dashboard_uid: str = typer.Option("thread-debugger", "--dashboard-uid", help="Dashboard UID"),
    from_range: str = typer.Option("now-24h", "--from", help="Grafana from range"),
    to_range: str = typer.Option("now", "--to", help="Grafana to range"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Build a direct Grafana thread-debugger URL from Slack thread input."""
    client = get_client()
    result = client.thread_debug_url(
        thread=thread,
        dashboard_uid=dashboard_uid,
        from_range=from_range,
        to_range=to_range,
    )

    if json_output:
        print(json.dumps(result, indent=2))
        return

    console.print(f"[bold]Thread Key:[/] {result['thread_key']}")
    console.print(f"[bold]Dashboard:[/] {result['dashboard_uid']}")
    console.print(f"[bold]URL:[/] {result['url']}")


@app.command()
def health():
    """Assert Grafana API connectivity and auth."""
    try:
        details = get_client().health()
    except Exception as exc:
        print(
            json.dumps(
                {"ok": False, "tool": "grafana", "error": str(exc), "details": {}},
                indent=2,
                default=str,
            )
        )
        raise typer.Exit(1) from exc
    payload = {"ok": True, "tool": "grafana", "error": None, "details": details}
    print(json.dumps(payload, indent=2, default=str))


if __name__ == "__main__":
    app()
