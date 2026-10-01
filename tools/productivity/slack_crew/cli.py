"""Thin agent-facing CLI for named Slack identities."""

import json

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
    json_output: bool = typer.Option(False, "--json"),
    markdown: bool = typer.Option(False, "--markdown"),
) -> None:
    """Update your name and description; other bots and permissions are inaccessible."""
    if name is None and description is None:
        raise typer.BadParameter("Provide --name or --description")
    output(SlackCrewClient().edit(name, description), markdown)
