"""CLI for browsing Sentry issues and events (read-only)."""

import json
from collections.abc import Callable
from typing import Any

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="sentry",
    help="Sentry issues — list/search issues, issue details, events, stacktraces, and tag values (read-only)",
    no_args_is_help=True,
)


@app.callback()
def main() -> None:
    """sentry CLI."""


def get_client():
    from .client import _client

    return _client()


def _emit(result: Any) -> None:
    print(json.dumps(result, indent=2, ensure_ascii=False, default=str))


def _run(operation: Callable[[Any], Any]) -> None:
    client = get_client()
    try:
        _emit(operation(client))
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()


@app.command("health")
def health():
    """Assert sentry connectivity and auth with a safe read-only check."""
    client = get_client()
    try:
        details = client.list_organizations()
        payload = {"ok": True, "tool": "sentry", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "sentry", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


@app.command("list-organizations")
def list_organizations() -> None:
    """List Sentry organizations available to the configured token."""
    _run(lambda client: client.list_organizations())


@app.command("list-projects")
def list_projects(
    organization_slug: str = typer.Argument(..., help="Sentry organization slug"),
) -> None:
    """List projects in a Sentry organization."""
    _run(lambda client: client.list_projects(organization_slug))


@app.command("list-issues")
def list_issues(
    organization_slug: str = typer.Argument(..., help="Sentry organization slug"),
    project_slug: str | None = typer.Option(
        None, "--project", "-p", help="Restrict results to this project slug"
    ),
    query: str = typer.Option(
        "is:unresolved", "--query", "-q", help="Native Sentry issue search query"
    ),
    sort: str = typer.Option("date", "--sort", help="Sort by date, new, freq, user, or priority"),
    stats_period: str = typer.Option(
        "14d", "--stats-period", help="Relative search window, e.g. 24h, 14d, or 90d"
    ),
    limit: int = typer.Option(25, "--limit", "-n", min=1, help="Maximum issues to return"),
) -> None:
    """List or search issues using Sentry's native query syntax."""
    _run(
        lambda client: client.list_issues(
            organization_slug=organization_slug,
            project_slug=project_slug,
            query=query,
            sort=sort,
            stats_period=stats_period,
            limit=limit,
        )
    )


@app.command("get-issue")
def get_issue(
    organization_slug: str = typer.Argument(..., help="Sentry organization slug"),
    issue_id: str = typer.Argument(..., help="Numeric issue id or short id"),
) -> None:
    """Get details for one Sentry issue."""
    _run(lambda client: client.get_issue(organization_slug, issue_id))


@app.command("list-issue-events")
def list_issue_events(
    organization_slug: str = typer.Argument(..., help="Sentry organization slug"),
    issue_id: str = typer.Argument(..., help="Numeric issue id or short id"),
    full: bool = typer.Option(False, "--full", help="Include each full event payload"),
    limit: int = typer.Option(25, "--limit", "-n", min=1, help="Maximum events to return"),
) -> None:
    """List individual events for a Sentry issue."""
    _run(
        lambda client: client.list_issue_events(
            organization_slug=organization_slug,
            issue_id=issue_id,
            full=full,
            limit=limit,
        )
    )


@app.command("get-event")
def get_event(
    organization_slug: str = typer.Argument(..., help="Sentry organization slug"),
    issue_id: str = typer.Argument(..., help="Numeric issue id or short id"),
    event_id: str = typer.Option(
        "latest", "--event-id", "-e", help="Specific event id, latest, or oldest"
    ),
) -> None:
    """Get one full event, including stacktrace and breadcrumbs."""
    _run(lambda client: client.get_event(organization_slug, issue_id, event_id=event_id))


@app.command("get-issue-tag-values")
def get_issue_tag_values(
    organization_slug: str = typer.Argument(..., help="Sentry organization slug"),
    issue_id: str = typer.Argument(..., help="Numeric issue id or short id"),
    tag_key: str = typer.Argument(..., help="Tag key, e.g. release or browser"),
) -> None:
    """Get the value distribution for one tag on a Sentry issue."""
    _run(lambda client: client.get_issue_tag_values(organization_slug, issue_id, tag_key))


if __name__ == "__main__":
    app()
