"""CLI for interview prep briefs."""

from dotenv import load_dotenv

load_dotenv()

import json
import os
import sys

import typer
from rich.console import Console

from .client import InterviewPrepClient

app = typer.Typer(name="interview-prep", help="Permission-gated interview prep briefs")
console = Console()


def _render_markdown(data: dict) -> str:
    if not data.get("access_granted"):
        return (
            f"Access denied for {data.get('requester_email') or 'unknown requester'}: "
            f"{data.get('reason')}"
        )

    lines = []
    candidate = data["candidate"]["name"]
    lines.append(f"**Interview prep: {candidate}**")
    lines.append("")
    if data.get("upcoming_interviews"):
        lines.append("**Upcoming interview**")
        for event in data["upcoming_interviews"]:
            lines.append(f"- {event['start']}: {event['format']}")
    else:
        lines.append("**Upcoming interview:** none found on the interviews calendar.")
    lines.append("")
    lines.append(f"**Background:** {data['background_summary']}")
    previous = data.get("previous_interviews") or []
    lines.append("")
    lines.append(
        "**Previous interviews:** " + (", ".join(previous) if previous else "none visible in Ashby/calendar.")
    )
    lines.append("")
    lines.append(f"**Cover:** {data['focus']}")
    return "\n".join(lines)


@app.command()
def brief(
    candidate_name: str = typer.Argument(..., help="Candidate name, e.g. 'Lot Kwarteng'"),
    slack_user_id: str | None = typer.Option(
        None,
        "--slack-user-id",
        help="Slack user ID of requester. Defaults to SLACK_REQUESTER_ID.",
    ),
    requester_email: str | None = typer.Option(
        None,
        "--requester-email",
        help="Requester email override for tests/admin use.",
    ),
    days_ahead: int = typer.Option(30, "--days-ahead", help="Upcoming schedule window"),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
):
    """Generate a brief after checking requester access."""
    slack_user_id = slack_user_id or os.environ.get("SLACK_REQUESTER_ID", "").strip() or None
    client = InterviewPrepClient()
    try:
        data = client.brief(
            candidate_name,
            slack_user_id=slack_user_id,
            requester_email=requester_email,
            days_ahead=days_ahead,
        )
    finally:
        client.close()

    if json_output:
        print(json.dumps(data, indent=2, default=str), file=sys.stdout)
        raise typer.Exit(0 if data.get("access_granted") else 1)

    console.print(_render_markdown(data))
    raise typer.Exit(0 if data.get("access_granted") else 1)


if __name__ == "__main__":
    app()
