"""CLI for the pond session archive."""

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="pond-recall",
    help=(
        "Recall past agent work: search the shared archive of harness sessions "
        "(Claude Code, Codex, pi) synced from every sandbox, then pull full "
        "transcripts or single messages by id."
    ),
)


def get_client():
    from .client import PondClient

    return PondClient()


@app.command()
def search(
    query: str = typer.Argument(..., help="What to search for (concepts and keywords)"),
    limit: int = typer.Option(10, "--limit", "-n", help="Max sessions to return"),
    project: str = typer.Option(None, "--project", help="Filter: project path substring"),
    session_id: str = typer.Option(None, "--session-id", help="Filter: one session, exact id"),
    from_date: str = typer.Option(None, "--from-date", help="Only on/after this date (YYYY-MM-DD)"),
    to_date: str = typer.Option(None, "--to-date", help="Only on/before this date (YYYY-MM-DD)"),
    sort_by: str = typer.Option("relevance", "--sort-by", help="relevance or recency"),
):
    """Search archived sessions; hits carry message/session ids for get commands."""
    print(
        get_client().search(
            query,
            limit=limit,
            project=project,
            session_id=session_id,
            from_date=from_date,
            to_date=to_date,
            sort_by=sort_by,
        )
    )


@app.command()
def session(
    session_id: str = typer.Argument(..., help="Session id from a search hit"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max messages per page"),
    from_end: bool = typer.Option(False, "--from-end", help="Read the most recent messages"),
    after_message_id: str = typer.Option(
        None, "--after-message-id", help="Page forward from this message id"
    ),
):
    """Fetch a whole archived session as a readable transcript."""
    print(
        get_client().get_session(
            session_id, limit=limit, from_end=from_end, after_message_id=after_message_id
        )
    )


@app.command()
def message(
    message_id: str = typer.Argument(..., help="Message id from a search hit"),
    context: int = typer.Option(3, "--context", "-C", help="Neighbor messages each side"),
):
    """Fetch one message with full parts (tool calls/results) plus context."""
    print(get_client().get_message(message_id, context=context))


@app.command()
def status():
    """Archive statistics: session/message counts, adapters, last sync."""
    print(get_client().status())


if __name__ == "__main__":
    app()
