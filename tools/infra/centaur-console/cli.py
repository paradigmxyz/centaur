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
admin_app = typer.Typer(
    help="Manage principal roles. Works only in a DM or Console session of an active Console admin."
)
app.add_typer(admin_app, name="admin")

URL_OPTION = typer.Option(None, "--url", help="centaur-console base URL")
BEARER_TOKEN_OPTION = typer.Option(
    None,
    "--bearer-token",
    help="Local/debug bearer token override",
    envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
)


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
    cron_expression: str | None = typer.Option(
        None,
        "--cron",
        help="Optional five-field cron expression in Pacific Time",
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
    """Create an ad-hoc or scheduled task owned by the current Console user."""
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
        help='New five-field cron expression in Pacific Time; pass "" to clear',
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


@admin_app.command("principals")
def admin_principals(
    slack_channel_id: str | None = typer.Option(
        None, "--slack-channel", help="Only principals bound to this Slack channel ID"
    ),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """List principals."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_principals(slack_channel_id=slack_channel_id)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("principal")
def admin_principal(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Show a principal and the roles assigned to it."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_principal(principal)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("roles")
def admin_roles(
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """List every role that can be assigned."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_roles()
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("assign-role")
def admin_assign_role(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    role: str = typer.Argument(..., help="Role OID or foreign ID, such as tool-github"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Assign a role to a principal."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_assign_role(principal, role)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("unassign-role")
def admin_unassign_role(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    role: str = typer.Argument(..., help="Role OID or foreign ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Remove a role from a principal."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        client.admin_unassign_role(principal, role)
    console.print_json(
        json.dumps({"data": {"principal": principal, "role": role, "removed": True}})
    )


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
