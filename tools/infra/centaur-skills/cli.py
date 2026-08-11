"""CLI for Console-authored skills."""

from __future__ import annotations

import json

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="centaur-skills",
    help="Discover Console-authored skills available to this agent",
    no_args_is_help=True,
)


def get_client():
    from .client import SkillsClient

    return SkillsClient()


@app.command("search")
def search(
    query: str = typer.Argument(..., help="Task or capability to find guidance for"),
    limit: int = typer.Option(10, "--limit", "-n", min=1, max=20),
) -> None:
    """Search Console-authored skills relevant to a task."""
    with get_client() as client:
        results = client.search(query, limit=limit)
    print(json.dumps({"data": results}, indent=2, default=str))


@app.command("list")
def list_skills(
    scope: str | None = typer.Option(None, "--scope", help="private or shared"),
    limit: int = typer.Option(20, "--limit", "-n", min=1, max=20),
) -> None:
    """List skills available to the current sandbox principal."""
    normalized_scope = scope.strip().lower() if scope else None
    if normalized_scope not in {None, "private", "shared"}:
        raise typer.BadParameter("scope must be private or shared")
    with get_client() as client:
        results = client.list(scope=normalized_scope, limit=limit)
    print(json.dumps({"data": results}, indent=2, default=str))


@app.command("read")
def read(
    identifier: str = typer.Argument(..., help="Skill name or OID"),
) -> None:
    """Read the complete current SKILL.md for one skill."""
    with get_client() as client:
        result = client.read(identifier)

    document = str(result.get("document") or "")
    print(document, end="" if document.endswith("\n") else "\n")


if __name__ == "__main__":
    app()
