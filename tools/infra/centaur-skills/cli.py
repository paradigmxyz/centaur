"""CLI for Console-authored skills."""

from __future__ import annotations

import json
from typing import Any

import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(
    name="centaur-skills",
    help="Discover Console-authored skills available to this agent",
    no_args_is_help=True,
)
console = Console()


def get_client():
    from .client import SkillsClient

    return SkillsClient()


def _validate_output_flags(json_output: bool, markdown_output: bool) -> None:
    if json_output and markdown_output:
        raise typer.BadParameter("choose either --json or --markdown")


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
        print("| OID | Name | Visibility | Description |")
        print("| --- | --- | --- | --- |")
        for skill in results:
            description = str(skill.get("description") or "").replace("|", "\\|")
            print(f"| {skill.get('id', '')} | {skill.get('name', '')} | {skill.get('visibility', '')} | {description} |")
        return

    table = Table(show_header=True, header_style="bold")
    table.add_column("OID", style="cyan", overflow="fold")
    table.add_column("Name", style="bold")
    table.add_column("Visibility")
    table.add_column("Description", overflow="fold")
    for skill in results:
        table.add_row(
            str(skill.get("id") or ""),
            str(skill.get("name") or ""),
            str(skill.get("visibility") or ""),
            str(skill.get("description") or ""),
        )
    console.print(table)


@app.command("search")
def search(
    query: str = typer.Argument(..., help="Task or capability to find guidance for"),
    limit: int = typer.Option(10, "--limit", "-n", min=1, max=20),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output a Markdown table"),
) -> None:
    """Search Console-authored skills relevant to a task."""
    _validate_output_flags(json_output, markdown_output)
    with get_client() as client:
        results = client.search(query, limit=limit)
    _print_skill_results(results, json_output=json_output, markdown_output=markdown_output)


@app.command("list")
def list_skills(
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
        results = client.list(scope=normalized_scope, limit=limit)
    _print_skill_results(results, json_output=json_output, markdown_output=markdown_output)


@app.command("read")
def read(
    identifier: str = typer.Argument(..., help="Skill name or OID"),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output the SKILL.md content"),
) -> None:
    """Read the complete current SKILL.md for one skill."""
    _validate_output_flags(json_output, markdown_output)
    with get_client() as client:
        result = client.read(identifier)

    if json_output:
        print(json.dumps({"data": result}, indent=2, default=str))
    else:
        document = str(result.get("document") or "")
        print(document, end="" if document.endswith("\n") else "\n")


if __name__ == "__main__":
    app()
