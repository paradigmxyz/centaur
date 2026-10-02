"""Thin agent-facing CLI for named Slack identities."""

import json
from pathlib import Path
from typing import Annotated

import typer

from .client import SlackCrewClient

app = typer.Typer(help="Read and edit your own Crew bot. Operators manage the crew in Console.")


def output(data: dict, markdown: bool) -> None:
    text = json.dumps(data, indent=2)
    typer.echo(f"```json\n{text}\n```" if markdown else text)


@app.command()
def me(
    json_output: bool = typer.Option(False, "--json"),
    markdown: bool = typer.Option(False, "--markdown"),
) -> None:
    """Read your own Crew profile."""
    output(SlackCrewClient().me(), markdown)


@app.command()
def edit(
    name: str | None = typer.Option(None, "--name"),
    description: str | None = typer.Option(None, "--description"),
    icon_url: str | None = typer.Option(
        None, "--icon-url", help="Public HTTPS profile picture, 512 to 2000 pixels per side."
    ),
    prompt_file: Annotated[
        Path | None, typer.Option("--prompt-file", exists=True, dir_okay=False)
    ] = None,
    skills_file: Annotated[
        Path | None, typer.Option("--skills-file", exists=True, dir_okay=False)
    ] = None,
    codex_model: str | None = typer.Option(None, "--codex-model"),
    claude_model: str | None = typer.Option(None, "--claude-model"),
    json_output: bool = typer.Option(False, "--json"),
    markdown: bool = typer.Option(False, "--markdown"),
) -> None:
    """Edit yourself. Skills JSON replaces your skills: [{name, description, content}].

    Behavior applies to newly created or rebuilt sandboxes. Roles are admin-only.
    """
    if all(
        value is None
        for value in (
            name,
            description,
            icon_url,
            prompt_file,
            skills_file,
            codex_model,
            claude_model,
        )
    ):
        raise typer.BadParameter("Provide at least one field to edit")
    skills = None
    if skills_file:
        try:
            skills = json.loads(skills_file.read_text())
        except (ValueError, OSError) as error:
            raise typer.BadParameter("Skills file must contain a JSON array") from error
        if not isinstance(skills, list):
            raise typer.BadParameter("Skills file must contain a JSON array")
    output(
        SlackCrewClient().edit(
            name,
            description,
            icon_url=icon_url,
            system_prompt=prompt_file.read_text() if prompt_file else None,
            skills=skills,
            codex_model=codex_model,
            claude_model=claude_model,
        ),
        markdown,
    )
