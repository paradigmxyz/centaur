"""CLI for Console-authored skills."""

from __future__ import annotations

import json
from pathlib import Path

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="centaur-skills",
    help="Discover, author, and manage editors for Console skills available to this agent",
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


@app.command("create")
def create(
    name: str = typer.Argument(..., help="Unique lowercase skill name"),
    description: str = typer.Option(..., "--description", "-d", help="When to use the skill"),
    instructions: str | None = typer.Option(
        None,
        "--instructions",
        "-i",
        help="Markdown instruction body",
    ),
    instructions_file: Path | None = typer.Option(  # noqa: B008
        None,
        "--instructions-file",
        exists=True,
        dir_okay=False,
        readable=True,
        help="Read the Markdown instruction body from this file",
    ),
) -> None:
    """Create a shared Console-authored skill."""
    resolved_instructions = _resolve_instructions(instructions, instructions_file, required=True)
    assert resolved_instructions is not None
    with get_client() as client:
        result = client.create(name, description, resolved_instructions)
    print(json.dumps({"data": result}, indent=2, default=str))


@app.command("edit")
def edit(
    identifier: str = typer.Argument(..., help="OID of an owned or editable skill"),
    name: str | None = typer.Option(None, "--name", help="New lowercase skill name"),
    description: str | None = typer.Option(
        None,
        "--description",
        "-d",
        help="New description",
    ),
    instructions: str | None = typer.Option(
        None,
        "--instructions",
        "-i",
        help="New Markdown instruction body",
    ),
    instructions_file: Path | None = typer.Option(  # noqa: B008
        None,
        "--instructions-file",
        exists=True,
        dir_okay=False,
        readable=True,
        help="Read the new Markdown instruction body from this file",
    ),
    lock_version: int | None = typer.Option(
        None,
        "--lock-version",
        min=0,
        help="Reject the edit if the skill has changed since this version",
    ),
) -> None:
    """Edit fields on an owned or editable Console-authored skill."""
    resolved_instructions = _resolve_instructions(instructions, instructions_file, required=False)
    if all(value is None for value in (name, description, resolved_instructions)):
        raise typer.BadParameter("provide --name, --description, or instructions to edit")

    with get_client() as client:
        result = client.edit(
            identifier,
            name=name,
            description=description,
            instructions=resolved_instructions,
            lock_version=lock_version,
        )
    print(json.dumps({"data": result}, indent=2, default=str))


@app.command("delete")
def delete(
    identifier: str = typer.Argument(..., help="OID of an owned skill"),
) -> None:
    """Archive an owned Console-authored skill."""
    with get_client() as client:
        client.delete(identifier)
    print(json.dumps({"data": {"id": identifier, "archived": True}}, indent=2))


@app.command("editors")
def editors(
    identifier: str = typer.Argument(..., help="Visible skill name or OID"),
) -> None:
    """List editors for a visible skill."""
    with get_client() as client:
        result = client.list_editors(identifier)
    print(json.dumps({"data": result}, indent=2, default=str))


@app.command("add-editor")
def add_editor(
    identifier: str = typer.Argument(..., help="OID of an owned skill"),
    user: str = typer.Argument(..., help="Exact Console user email or usr_ OID"),
) -> None:
    """Add an editor to an owned Console-authored skill."""
    with get_client() as client:
        result = client.add_editor(identifier, user)
    print(json.dumps({"data": result}, indent=2, default=str))


@app.command("remove-editor")
def remove_editor(
    identifier: str = typer.Argument(..., help="OID of an owned skill"),
    user: str = typer.Argument(..., help="Exact Console user email or usr_ OID"),
) -> None:
    """Remove an editor from an owned Console-authored skill."""
    with get_client() as client:
        result = client.remove_editor(identifier, user)
    print(json.dumps({"data": result}, indent=2, default=str))


def _resolve_instructions(
    instructions: str | None,
    instructions_file: Path | None,
    *,
    required: bool,
) -> str | None:
    if instructions is not None and instructions_file is not None:
        raise typer.BadParameter("use only one of --instructions or --instructions-file")
    if instructions_file is not None:
        return instructions_file.read_text(encoding="utf-8")
    if instructions is not None:
        return instructions
    if required:
        raise typer.BadParameter("provide --instructions or --instructions-file")
    return None


if __name__ == "__main__":
    app()
