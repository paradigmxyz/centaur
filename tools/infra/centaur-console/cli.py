"""CLI for centaur-console sandbox permission introspection."""

from __future__ import annotations

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

load_dotenv()

app = typer.Typer(
    name="centaur-console",
    help="Inspect Console permissions and manage the current user's scheduled tasks",
)
console = Console()


def get_client(
    url: str | None = None,
    bearer_token: str | None = None,
):
    from .client import ConsoleClient

    return ConsoleClient(url=url, bearer_token=bearer_token)


@app.command("permissions")
def permissions(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Print the current sandbox's redacted permissions as JSON."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.sandbox_permissions()
    console.print_json(json.dumps(result, default=str))


@app.command("oauth-apps")
def oauth_apps(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Print enabled OAuth apps and their consent start URLs as JSON."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.sandbox_oauth_apps()
    console.print_json(json.dumps({"data": result}, default=str))


@app.command("tasks")
def scheduled_tasks(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
) -> None:
    """List scheduled tasks owned by the current Console user."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.scheduled_tasks()
    console.print_json(json.dumps({"data": result}, default=str))


@app.command("task")
def scheduled_task(
    task_id: str = typer.Argument(..., help="Scheduled task OID"),
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
) -> None:
    """Read one scheduled task owned by the current Console user."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.scheduled_task(task_id)
    console.print_json(json.dumps({"data": result}, default=str))


@app.command("create-task")
def create_scheduled_task(
    name: str = typer.Argument(..., help="Unique task name"),
    prompt: str = typer.Option(..., "--prompt", "-p", help="Prompt to run"),
    cron_expression: str = typer.Option(
        ...,
        "--cron",
        help="Five-field cron expression in Pacific Time",
    ),
    delivery_channel: str = typer.Option(
        "dm",
        "--delivery-channel",
        help="dm or a permitted Slack channel ID",
    ),
    enabled: bool = typer.Option(True, "--enabled/--disabled"),
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
) -> None:
    """Create a scheduled task owned by the current Console user."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.create_scheduled_task(
            name=name,
            prompt=prompt,
            cron_expression=cron_expression,
            delivery_channel=delivery_channel,
            enabled=enabled,
        )
    console.print_json(json.dumps({"data": result}, default=str))


@app.command("update-task")
def update_scheduled_task(
    task_id: str = typer.Argument(..., help="Scheduled task OID"),
    name: str | None = typer.Option(None, "--name", help="New task name"),
    prompt: str | None = typer.Option(None, "--prompt", "-p", help="New prompt"),
    cron_expression: str | None = typer.Option(
        None,
        "--cron",
        help="New five-field cron expression in Pacific Time",
    ),
    delivery_channel: str | None = typer.Option(
        None,
        "--delivery-channel",
        help="dm or a permitted Slack channel ID",
    ),
    enabled: bool | None = typer.Option(None, "--enabled/--disabled"),
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
) -> None:
    """Update selected fields on a scheduled task."""
    if all(value is None for value in (name, prompt, cron_expression, delivery_channel, enabled)):
        raise typer.BadParameter("provide at least one task field to update")

    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.update_scheduled_task(
            task_id,
            name=name,
            prompt=prompt,
            cron_expression=cron_expression,
            delivery_channel=delivery_channel,
            enabled=enabled,
        )
    console.print_json(json.dumps({"data": result}, default=str))


@app.command("delete-task")
def delete_scheduled_task(
    task_id: str = typer.Argument(..., help="Scheduled task OID"),
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
) -> None:
    """Delete a scheduled task owned by the current Console user."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        client.delete_scheduled_task(task_id)
    console.print_json(json.dumps({"data": {"id": task_id, "deleted": True}}))


@app.command("run-task")
def run_scheduled_task(
    task_id: str = typer.Argument(..., help="Scheduled task OID"),
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
) -> None:
    """Queue an immediate run of a scheduled task."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.run_scheduled_task(task_id)
    console.print_json(json.dumps({"data": result}, default=str))


@app.command()
def health(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Assert the sandbox permissions endpoint is reachable and authorized."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        payload = client.health()
    print(json.dumps(payload, indent=2, default=str))
    if not payload.get("ok"):
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
