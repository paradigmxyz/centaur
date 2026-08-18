"""CLI for AWS CloudWatch (read-only).

Mirrors the other infra tools' CLIs (grafana, vlogs): a Typer app whose
commands wrap ``CloudWatchClient`` and print JSON to stdout. The agent reaches
this via the ``cloudwatch`` shim that ``install_tool_shims.py`` installs from
``[project.scripts]``; without that entry the tool is invisible on the
shim-based (api-rs) path even though its ``[tool.centaur]`` metadata loads.
"""

import json

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="cloudwatch",
    help="AWS CloudWatch CLI — Logs Insights, log events, metrics, and alarms (read-only)",
    no_args_is_help=True,
)


@app.command("health")
def health():
    """Assert cloudwatch connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.list_log_groups(limit=1)
        payload = {"ok": True, "tool": "cloudwatch", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "cloudwatch", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


def get_client():
    from .client import CloudWatchClient

    return CloudWatchClient()


def _emit(result) -> None:
    # default=str keeps any stray datetime/Decimal JSON-serializable.
    print(json.dumps(result, indent=2, default=str))


@app.command("list-log-groups")
def list_log_groups(
    prefix: str = typer.Option(None, "--prefix", "-p", help="Filter by log group name prefix"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max groups (1-50)"),
):
    """List CloudWatch log groups (discover names for filter/start-query)."""
    _emit(get_client().list_log_groups(name_prefix=prefix, limit=limit))


@app.command("filter-log-events")
def filter_log_events(
    log_group_name: str = typer.Argument(..., help="Exact log group name"),
    filter_pattern: str = typer.Option(
        None, "--filter", "-f", help="CloudWatch Logs filter pattern (e.g. ERROR)"
    ),
    start: str = typer.Option(
        None, "--start", "-s", help="Start (ISO-8601 or epoch); default 1h before end"
    ),
    end: str = typer.Option(None, "--end", "-e", help="End (ISO-8601 or epoch); default now"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max events (1-10000)"),
):
    """Search log events in a group within a time window."""
    _emit(
        get_client().filter_log_events(
            log_group_name=log_group_name,
            filter_pattern=filter_pattern,
            start_time=start,
            end_time=end,
            limit=limit,
        )
    )


@app.command("start-query")
def start_query(
    query_string: str = typer.Argument(..., help="Logs Insights query string"),
    log_group: list[str] = typer.Option(
        ..., "--log-group", "-g", help="Log group name (repeat for multiple)"
    ),
    start: str = typer.Option(None, "--start", "-s", help="Start (ISO-8601 or epoch)"),
    end: str = typer.Option(None, "--end", "-e", help="End (ISO-8601 or epoch)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max rows (1-10000)"),
):
    """Start a Logs Insights query; poll get-query-results with the returned query_id."""
    _emit(
        get_client().start_query(
            log_group_names=log_group,
            query_string=query_string,
            start_time=start,
            end_time=end,
            limit=limit,
        )
    )


@app.command("get-query-results")
def get_query_results(
    query_id: str = typer.Argument(..., help="query_id returned by start-query"),
):
    """Get results/status for a Logs Insights query (poll until status is Complete)."""
    _emit(get_client().get_query_results(query_id=query_id))


@app.command("stop-query")
def stop_query(
    query_id: str = typer.Argument(..., help="query_id returned by start-query"),
):
    """Stop a running Logs Insights query."""
    _emit(get_client().stop_query(query_id=query_id))


@app.command("list-metrics")
def list_metrics(
    namespace: str = typer.Option(None, "--namespace", "-N", help="e.g. AWS/EC2, AWS/Lambda"),
    metric: str = typer.Option(None, "--metric", "-m", help="e.g. CPUUtilization, Errors"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max metrics"),
):
    """List available metrics (discover namespace/name/dimensions for get-metric-data)."""
    _emit(get_client().list_metrics(namespace=namespace, metric_name=metric, limit=limit))


@app.command("get-metric-data")
def get_metric_data(
    namespace: str = typer.Argument(..., help="Metric namespace, e.g. AWS/Lambda"),
    metric_name: str = typer.Argument(..., help="Metric name, e.g. Errors"),
    dimensions: str = typer.Option(
        None, "--dimensions", "-d", help='JSON map, e.g. {"FunctionName":"my-fn"}'
    ),
    stat: str = typer.Option(
        "Average", "--stat", help="Average, Sum, Minimum, Maximum, SampleCount, p99, ..."
    ),
    period: int = typer.Option(300, "--period", help="Granularity seconds (multiple of 60)"),
    start: str = typer.Option(None, "--start", "-s", help="Start (ISO-8601 or epoch)"),
    end: str = typer.Option(None, "--end", "-e", help="End (ISO-8601 or epoch)"),
):
    """Fetch time-series data points for a single metric."""
    dims = json.loads(dimensions) if dimensions else None
    _emit(
        get_client().get_metric_data(
            namespace=namespace,
            metric_name=metric_name,
            dimensions=dims,
            stat=stat,
            period=period,
            start_time=start,
            end_time=end,
        )
    )


@app.command("describe-alarms")
def describe_alarms(
    state: str = typer.Option(None, "--state", help="OK, ALARM, or INSUFFICIENT_DATA"),
    prefix: str = typer.Option(None, "--prefix", "-p", help="Filter by alarm name prefix"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max alarms (1-100)"),
):
    """List metric alarms (pass --state ALARM for currently-firing alarms)."""
    _emit(get_client().describe_alarms(state_value=state, alarm_name_prefix=prefix, limit=limit))


@app.command("get-alarm-history")
def get_alarm_history(
    alarm: str = typer.Option(None, "--alarm", "-a", help="Restrict to one alarm name"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max items (1-100)"),
):
    """Get state-change history for an alarm (or across all alarms)."""
    _emit(get_client().get_alarm_history(alarm_name=alarm, limit=limit))


if __name__ == "__main__":
    app()
