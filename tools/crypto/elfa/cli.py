"""CLI for Elfa AI event summaries."""

import json

import typer
from dotenv import load_dotenv

from .client import DEFAULT_TIME_WINDOW, _client

load_dotenv()

app = typer.Typer(name="elfa", help="Elfa AI event summaries with source links.")


@app.callback()
def main() -> None:
    """Query Elfa AI."""


@app.command("event-summary")
def event_summary(
    keywords: str = typer.Argument(..., help="Keyword, e.g. ETH"),
    time_window: str = typer.Option(DEFAULT_TIME_WINDOW, help="Lookback window, e.g. 1h, 24h, 7d"),
    json_output: bool = typer.Option(True, "--json/--markdown", help="Output JSON or Markdown"),
) -> None:
    """Summarize recent keyword mentions with source links (5 API credits)."""
    with _client() as client:
        result = client.get_event_summary(keywords, time_window=time_window)

    if json_output:
        typer.echo(json.dumps(result, indent=2, ensure_ascii=False))
        return

    for event in result["data"]:
        typer.echo(event["summary"])
        for link in event["sourceLinks"]:
            typer.echo(f"- <{link}>")
        typer.echo()


if __name__ == "__main__":
    app()
