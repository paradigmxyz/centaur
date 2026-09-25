"""Command-line interface for Luma's public API."""

from __future__ import annotations

import json
from typing import Annotated, Any

import typer

from .client import LumaClient

app = typer.Typer(name="luma", help="Manage Luma calendars, events, guests, and webhooks")


def _print(value: object) -> None:
    typer.echo(json.dumps(value, indent=2, ensure_ascii=False, default=str))


def _json_object(value: str, option: str) -> dict[str, Any]:
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter(f"{option} must be valid JSON: {exc.msg}") from exc
    if not isinstance(parsed, dict):
        raise typer.BadParameter(f"{option} must be a JSON object")
    return parsed


@app.command()
def health() -> None:
    """Check API-key authentication with a safe read-only request."""
    try:
        with LumaClient() as client:
            details = client.get_self()
        payload = {"ok": True, "tool": "luma", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "luma", "error": str(exc), "details": {}}
        _print(payload)
        raise typer.Exit(1) from exc
    _print(payload)


@app.command()
def whoami() -> None:
    """Show the user associated with the API key."""
    with LumaClient() as client:
        _print(client.get_self())


@app.command()
def calendar(
    calendar_id: str | None = typer.Option(
        None, "--calendar-id", help="Calendar ID when using an organization API key."
    ),
) -> None:
    """Get the calendar associated with the API key."""
    with LumaClient() as client:
        _print(client.get_calendar(calendar_id=calendar_id))


@app.command()
def events(
    before: str | None = typer.Option(None, help="Only events before this ISO 8601 time."),
    after: str | None = typer.Option(None, help="Only events after this ISO 8601 time."),
    cursor: str | None = typer.Option(None, help="Cursor from a previous response."),
    limit: int | None = typer.Option(None, "--limit", "-n"),
    platform: Annotated[list[str] | None, typer.Option("--platform")] = None,
    access: Annotated[list[str] | None, typer.Option("--access")] = None,
    status: str | None = typer.Option(None),
    sort_direction: str | None = typer.Option(None, "--sort-direction"),
    calendar_id: str | None = typer.Option(None, "--calendar-id"),
) -> None:
    """List events on the calendar."""
    with LumaClient() as client:
        _print(
            client.list_events(
                before=before,
                after=after,
                cursor=cursor,
                limit=limit,
                platforms=platform,
                access=access,
                status=status,
                sort_direction=sort_direction,
                calendar_id=calendar_id,
            )
        )


@app.command()
def event(event_id: str) -> None:
    """Get one event by its evt- ID."""
    with LumaClient() as client:
        _print(client.get_event(event_id))


@app.command("create-event")
def create_event(
    data: str = typer.Option(..., "--data", help="EventCreateRequest as a JSON object."),
    calendar_id: str | None = typer.Option(None, "--calendar-id"),
) -> None:
    """Create an event."""
    with LumaClient() as client:
        _print(client.create_event(_json_object(data, "--data"), calendar_id=calendar_id))


@app.command("update-event")
def update_event(
    data: str = typer.Option(..., "--data", help="Event update, including event_id, as JSON."),
    calendar_id: str | None = typer.Option(None, "--calendar-id"),
) -> None:
    """Update an event."""
    with LumaClient() as client:
        _print(client.update_event(_json_object(data, "--data"), calendar_id=calendar_id))


@app.command()
def guests(
    event_id: str,
    approval_status: str | None = typer.Option(None, "--approval-status"),
    cursor: str | None = typer.Option(None),
    limit: int | None = typer.Option(None, "--limit", "-n"),
    sort_column: str | None = typer.Option(None, "--sort-column"),
    sort_direction: str | None = typer.Option(None, "--sort-direction"),
) -> None:
    """List guests for an event."""
    with LumaClient() as client:
        _print(
            client.list_guests(
                event_id,
                approval_status=approval_status,
                cursor=cursor,
                limit=limit,
                sort_column=sort_column,
                sort_direction=sort_direction,
            )
        )


@app.command("request")
def raw_request(
    method: str = typer.Argument(..., help="HTTP method, usually GET or POST."),
    endpoint: str = typer.Argument(..., help="Luma path beginning with /v1/ or /v2/."),
    params: str | None = typer.Option(None, "--params", help="Query parameters as JSON."),
    data: str | None = typer.Option(None, "--data", help="Request body as JSON."),
    calendar_id: str | None = typer.Option(None, "--calendar-id"),
) -> None:
    """Call any Luma endpoint, including contacts, tags, tickets, and webhooks."""
    parsed_params = _json_object(params, "--params") if params is not None else None
    parsed_data = _json_object(data, "--data") if data is not None else None
    with LumaClient() as client:
        _print(
            client.request(
                method,
                endpoint,
                params=parsed_params,
                json=parsed_data,
                calendar_id=calendar_id,
            )
        )


if __name__ == "__main__":
    app()
