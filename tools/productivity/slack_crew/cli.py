"""Thin agent-facing CLI for named Slack identities."""

import json

import typer

from .client import SlackCrewClient

app = typer.Typer(help="Create and inspect named Centaur Crew Slackbots")


def output(data: dict, markdown: bool) -> None:
    text = json.dumps(data, indent=2)
    typer.echo(f"```json\n{text}\n```" if markdown else text)


@app.command("list")
def list_bots(
    json_output: bool = typer.Option(False, "--json"),
    markdown: bool = typer.Option(False, "--markdown"),
) -> None:
    """List apps and their installation status."""
    output(SlackCrewClient().list_bots(), markdown)


@app.command()
def create(
    id: str,
    name: str = typer.Option(..., "--name"),
    crew: str = typer.Option(..., "--crew"),
    json_output: bool = typer.Option(False, "--json"),
    markdown: bool = typer.Option(False, "--markdown"),
) -> None:
    """Create an app once and return its Slack installation link."""
    output(SlackCrewClient().create_bot(id, name, crew), markdown)


@app.command()
def health(
    json_output: bool = typer.Option(False, "--json"),
    markdown: bool = typer.Option(False, "--markdown"),
) -> None:
    """Check management access without creating an app."""
    output(SlackCrewClient().health(), markdown)
