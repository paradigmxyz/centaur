"""CLI for linting drafts for wordiness, mannered prose, hedging, and citations."""

import json
import sys

import typer

from .client import _client

app = typer.Typer(
    name="prose-lint",
    help="Score drafts for wordiness, mannered prose, hedging, and citations.",
)


@app.callback()
def main() -> None:
    """Lint drafts for wordiness, mannered prose, hedging, and citations."""


@app.command()
def score(
    text: str | None = typer.Argument(None, help="Draft to score; read from stdin when omitted"),
    context: str | None = typer.Option(None, "--context", "-c", help="Request the draft answers"),
) -> None:
    """Score a draft and print the result as JSON."""
    try:
        result = _client().score(
            text if text is not None else sys.stdin.read(),
            context=context,
        )
    except (RuntimeError, ValueError) as exc:
        print(json.dumps({"ok": False, "error": str(exc)}), file=sys.stderr)
        raise typer.Exit(1) from exc
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    app()
