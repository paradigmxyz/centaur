"""Command-line interface for Pangram detection APIs."""

from __future__ import annotations

import json

import httpx
import typer

from .client import (
    DEFAULT_MODEL,
    DEFAULT_POLL_INTERVAL_SECONDS,
    DEFAULT_TASK_TIMEOUT_SECONDS,
    _client,
)

app = typer.Typer(name="pangram", help="Detect AI-generated or plagiarized text with Pangram.")


def _echo(payload: dict) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))


def _error_message(exc: Exception) -> str:
    if isinstance(exc, httpx.HTTPStatusError):
        return f"Pangram API error: HTTP {exc.response.status_code}"
    if isinstance(exc, httpx.HTTPError):
        return f"Pangram request failed ({type(exc).__name__})."
    return str(exc)


def _run(operation) -> dict:
    try:
        with _client() as client:
            return operation(client)
    except (httpx.HTTPError, KeyError, RuntimeError, TimeoutError, ValueError) as exc:
        typer.echo(_error_message(exc), err=True)
        raise typer.Exit(1) from None


@app.command()
def health() -> None:
    """Check Pangram authentication and connectivity by listing models."""
    try:
        with _client() as client:
            details = client.list_models()
    except (httpx.HTTPError, KeyError, RuntimeError, ValueError) as exc:
        _echo(
            {
                "ok": False,
                "tool": "pangram",
                "error": _error_message(exc),
                "details": {},
            }
        )
        raise typer.Exit(1) from None
    _echo({"ok": True, "tool": "pangram", "error": None, "details": details})


@app.command()
def models() -> None:
    """List AI-detection models available to the API key."""
    _echo(_run(lambda client: client.list_models()))


@app.command()
def detect(
    text: str = typer.Argument(..., help="Text to analyze."),
    model: str = typer.Option(DEFAULT_MODEL, help="Model selector returned by `pangram models`."),
    public_dashboard_link: bool = typer.Option(
        False,
        "--public-dashboard-link",
        help="Request a public dashboard link.",
    ),
    timeout: float = typer.Option(
        DEFAULT_TASK_TIMEOUT_SECONDS,
        min=0.01,
        help="Maximum seconds to wait for the task.",
    ),
    poll_interval: float = typer.Option(
        DEFAULT_POLL_INTERVAL_SECONDS,
        "--poll-interval",
        min=0.01,
        help="Seconds between status checks.",
    ),
) -> None:
    """Analyze text and wait for the AI-detection result."""
    _echo(
        _run(
            lambda client: client.detect(
                text,
                model=model,
                public_dashboard_link=public_dashboard_link,
                task_timeout=timeout,
                poll_interval=poll_interval,
            )
        )
    )


@app.command("task")
def task_status(task_id: str = typer.Argument(..., help="Task ID returned by Pangram.")) -> None:
    """Fetch an existing AI-detection task's status or result."""
    _echo(_run(lambda client: client.get_task(task_id)))


@app.command()
def plagiarism(text: str = typer.Argument(..., help="Text to check for plagiarism.")) -> None:
    """Check text for potential plagiarism against online content."""
    _echo(_run(lambda client: client.check_plagiarism(text)))


if __name__ == "__main__":
    app()
