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
    help=(
        "Create Slack and workflow principals and static or pg_dsn secrets, and manage roles, "
        "secret grants, Slack permissions, sandbox access, "
        "and allowed Slack Connect workspaces. "
        "Works only in an active Console admin's Slack DM."
    )
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


@admin_app.command("create-principal")
def admin_create_principal(
    slack_channel_id: str | None = typer.Option(
        None,
        "--slack-channel",
        help="Slack channel ID (C… or G…) to create a channel principal for",
    ),
    slack_user_id: str | None = typer.Option(
        None,
        "--slack-user",
        help="Slack user ID in your workspace to create a user (DM) principal for",
    ),
    workflow_name: str | None = typer.Option(
        None,
        "--workflow",
        help="WORKFLOW_NAME to create the workflow-<name> principal for",
    ),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Create a Slack channel, Slack user, or workflow principal."""
    if sum(value is not None for value in (slack_channel_id, slack_user_id, workflow_name)) != 1:
        raise typer.BadParameter("pass exactly one of --slack-channel, --slack-user, or --workflow")
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_create_principal(
            slack_channel_id=slack_channel_id,
            slack_user_id=slack_user_id,
            workflow_name=workflow_name,
        )
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("principal")
def admin_principal(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Show a principal with its roles, direct grants, Slack permissions, and sandbox access."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_principal(principal)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("effective-grants")
def admin_effective_grants(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """List secrets a principal holds directly or through roles, and how each is granted."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_effective_grants(principal)
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


@admin_app.command("secrets")
def admin_secrets(
    query: str | None = typer.Option(None, "--query", "-q", help="Name or foreign ID filter"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """List secrets that can be granted."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_secrets(query)
    console.print_json(json.dumps({"data": result}, default=str))


def _json_object(value: str) -> dict:
    try:
        data = json.loads(value)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter(f"--data is not valid JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise typer.BadParameter("--data must be a JSON object")
    return data


@admin_app.command("secret")
def admin_secret(
    secret: str = typer.Argument(..., help="Static or pg_dsn secret OID, foreign ID, or name"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Show a static or pg_dsn secret's document, including its source reference (never its value)."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_secret_document(secret)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("create-secret")
def admin_create_secret(
    kind: str = typer.Option("static", "--kind", help="static or pg_dsn"),
    data: str = typer.Option(..., "--data", help="JSON Console API document for the secret"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Create a static or pg_dsn secret. Sources must reference an external store."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_create_secret(kind, _json_object(data))
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("update-secret")
def admin_update_secret(
    secret: str = typer.Argument(..., help="Static or pg_dsn secret OID, foreign ID, or name"),
    data: str = typer.Option(..., "--data", help="JSON object of fields to replace"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Replace the given fields of a secret; omitted fields, including its source, are kept."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_update_secret(secret, _json_object(data))
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("grant-secret")
def admin_grant_secret(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    secret: str = typer.Argument(..., help="Secret OID, foreign ID, or name"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Grant a secret directly to a principal."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_grant_secret(principal, secret)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("revoke-secret")
def admin_revoke_secret(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    secret: str = typer.Argument(..., help="Secret OID, foreign ID, or name"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Revoke a secret granted directly to a principal."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        client.admin_revoke_secret(principal, secret)
    console.print_json(
        json.dumps({"data": {"principal": principal, "secret": secret, "revoked": True}})
    )


@admin_app.command("set-slack-permission")
def admin_set_slack_permission(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    channel_id: str = typer.Argument(..., help="Slack channel ID"),
    upload: bool | None = typer.Option(None, "--upload/--no-upload"),
    download: bool | None = typer.Option(None, "--download/--no-download"),
    history: bool | None = typer.Option(None, "--history/--no-history"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Create or update a Slack channel permission. New ones enable unspecified flags."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_set_slack_permission(
            principal, channel_id, upload=upload, download=download, history=history
        )
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("remove-slack-permission")
def admin_remove_slack_permission(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    channel_id: str = typer.Argument(..., help="Slack channel ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Remove a principal's own permission for a Slack channel."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        client.admin_remove_slack_permission(principal, channel_id)
    console.print_json(
        json.dumps({"data": {"principal": principal, "channel_id": channel_id, "removed": True}})
    )


@admin_app.command("set-sandbox-access")
def admin_set_sandbox_access(
    principal: str = typer.Argument(..., help="Principal OID or foreign ID"),
    repo_cache: str | None = typer.Option(None, "--repo-cache", help="none, public, or all"),
    observability: bool | None = typer.Option(None, "--observability/--no-observability"),
    sessions_read: bool | None = typer.Option(None, "--sessions-read/--no-sessions-read"),
    workflows_read: bool | None = typer.Option(None, "--workflows-read/--no-workflows-read"),
    workflows_write: bool | None = typer.Option(None, "--workflows-write/--no-workflows-write"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Change selected sandbox capabilities; unspecified ones are unchanged."""
    capabilities = {
        "sandbox_repo_cache": repo_cache,
        "sandbox_observability_enabled": observability,
        "sandbox_sessions_read_enabled": sessions_read,
        "sandbox_workflows_read_enabled": workflows_read,
        "sandbox_workflows_write_enabled": workflows_write,
    }
    if all(value is None for value in capabilities.values()):
        raise typer.BadParameter("provide at least one sandbox capability")
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_set_sandbox_access(principal, **capabilities)
    console.print_json(json.dumps({"data": result}, default=str))


@admin_app.command("slack-connect-teams")
def admin_slack_connect_teams(
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """List the Slack Connect workspace and org ids Centaur accepts messages from."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_slack_external_teams()
    console.print_json(json.dumps({"data": result}))


@admin_app.command("add-slack-connect-team")
def admin_add_slack_connect_team(
    team_id: str = typer.Argument(..., help="Slack workspace (T…) or Enterprise Grid org (E…) ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Allow messages from a Slack Connect workspace or org; prints the updated list."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_add_slack_external_team(team_id)
    console.print_json(json.dumps({"data": result}))


@admin_app.command("remove-slack-connect-team")
def admin_remove_slack_connect_team(
    team_id: str = typer.Argument(..., help="Slack workspace (T…) or Enterprise Grid org (E…) ID"),
    url: str | None = URL_OPTION,
    bearer_token: str | None = BEARER_TOKEN_OPTION,
) -> None:
    """Stop accepting messages from a Slack Connect workspace or org; prints the updated list."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.admin_remove_slack_external_team(team_id)
    console.print_json(json.dumps({"data": result}))


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
