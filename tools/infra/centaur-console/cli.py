"""CLI for centaur-console sandbox permission introspection."""

from __future__ import annotations

import json
from typing import Any

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(
    name="centaur-console",
    help="Inspect the current sandbox's centaur-console permissions",
)
console = Console()
skills_app = typer.Typer(
    name="centaur-skills",
    help="Discover Console-authored skills available to this agent",
    no_args_is_help=True,
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


def _validate_output_flags(json_output: bool, markdown_output: bool) -> None:
    if json_output and markdown_output:
        raise typer.BadParameter("choose either --json or --markdown")


def _skill_identifier(skill: dict[str, Any]) -> str:
    return str(skill.get("id") or "")


def _print_skill_results(
    results: list[dict[str, Any]],
    *,
    json_output: bool,
    markdown_output: bool,
) -> None:
    if json_output:
        print(json.dumps({"data": results}, indent=2, default=str))
        return
    if markdown_output:
        print("| Identifier | Name | Visibility | Description |")
        print("| --- | --- | --- | --- |")
        for skill in results:
            description = str(skill.get("description") or "").replace("|", "\\|")
            print(f"| {_skill_identifier(skill)} | {skill.get('name', '')} | {skill.get('visibility', '')} | {description} |")
        return

    table = Table(show_header=True, header_style="bold")
    table.add_column("Identifier", style="cyan", overflow="fold")
    table.add_column("Name", style="bold")
    table.add_column("Scope")
    table.add_column("Description", overflow="fold")
    for skill in results:
        table.add_row(
            _skill_identifier(skill),
            str(skill.get("name") or ""),
            str(skill.get("visibility") or ""),
            str(skill.get("description") or ""),
        )
    console.print(table)


@skills_app.command("search")
def skills_search(
    query: str = typer.Argument(..., help="Task or capability to find guidance for"),
    limit: int = typer.Option(10, "--limit", "-n", min=1, max=20),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output a Markdown table"),
) -> None:
    """Search Console-authored skills relevant to a task."""
    _validate_output_flags(json_output, markdown_output)
    with get_client() as client:
        results = client.skills_search(query, limit=limit)
    _print_skill_results(results, json_output=json_output, markdown_output=markdown_output)


@skills_app.command("list")
def skills_list(
    scope: str | None = typer.Option(None, "--scope", help="private or shared"),
    limit: int = typer.Option(20, "--limit", "-n", min=1, max=20),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output a Markdown table"),
) -> None:
    """List skills available to the current sandbox principal."""
    _validate_output_flags(json_output, markdown_output)
    normalized_scope = scope.strip().lower() if scope else None
    if normalized_scope not in {None, "private", "shared"}:
        raise typer.BadParameter("scope must be private or shared")
    with get_client() as client:
        results = client.skills_list(scope=normalized_scope, limit=limit)
    _print_skill_results(results, json_output=json_output, markdown_output=markdown_output)


@skills_app.command("read")
def skills_read(
    identifier: str = typer.Argument(..., help="Skill name or OID"),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output the SKILL.md content"),
) -> None:
    """Read the complete current SKILL.md for one skill."""
    _validate_output_flags(json_output, markdown_output)
    with get_client() as client:
        result = client.skill_read(identifier)

    if json_output:
        print(json.dumps({"data": result}, indent=2, default=str))
    else:
        document = str(result.get("document") or "")
        print(document, end="" if document.endswith("\n") else "\n")


if __name__ == "__main__":
    app()
