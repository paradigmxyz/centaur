"""Command-line interface for Mercator."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import httpx
import typer

from .client import DEFAULT_SEARCH_LIMIT, _client

app = typer.Typer(
    name="mercator",
    help="Find, quote, and pay for external API calls through Mercator.",
    epilog=(
        "Agent workflow: search for the complete outcome, describe an endpoint when its "
        "inputs are unclear, write a plan and quote it, then submit it with --max-spend and "
        "a new --idempotency-key. If submit fails, retry with the same key. Poll "
        "`mercator job JOB_ID` until the status is succeeded, partially_succeeded, or "
        "failed. Never resubmit a pending job.\n\n"
        "Plan format reference: https://mercator.sh/docs"
    ),
    no_args_is_help=True,
)


def _echo(payload: dict) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))


def _error_message(exc: Exception) -> str:
    if isinstance(exc, httpx.HTTPStatusError):
        return f"Mercator API error: HTTP {exc.response.status_code}"
    if isinstance(exc, httpx.HTTPError):
        return f"Mercator request failed ({type(exc).__name__})."
    return str(exc)


def _run(operation) -> dict:
    try:
        with _client() as client:
            return operation(client)
    except (httpx.HTTPError, KeyError, OSError, RuntimeError, ValueError) as exc:
        typer.echo(_error_message(exc), err=True)
        raise typer.Exit(1) from None


def _load_plan(path: str) -> dict[str, Any]:
    text = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
    plan = json.loads(text)
    if not isinstance(plan, dict):
        raise ValueError("Plan must be a JSON object with a nodes list.")
    return plan


@app.command()
def health() -> None:
    """Check Mercator authentication and wallet readiness without paying."""
    try:
        with _client() as client:
            details = client.connection_status()
    except (httpx.HTTPError, KeyError, RuntimeError, ValueError) as exc:
        _echo({"ok": False, "tool": "mercator", "error": _error_message(exc), "details": {}})
        raise typer.Exit(1) from None
    _echo({"ok": True, "tool": "mercator", "error": None, "details": details})


@app.command()
def search(
    query: str = typer.Argument(..., help="The complete outcome you need, in plain language."),
    limit: int = typer.Option(
        DEFAULT_SEARCH_LIMIT, "--limit", "-n", min=1, max=25, help="Endpoints to return."
    ),
) -> None:
    """Find and rank service endpoints for an outcome. Free."""
    _echo(_run(lambda client: client.search(query, limit=limit)))


@app.command()
def describe(
    service_id: str = typer.Argument(..., help="Service ID returned by `mercator search`."),
    method: str | None = typer.Option(None, help="Endpoint method; pass together with --path."),
    path: str | None = typer.Option(None, help="Endpoint path; pass together with --method."),
) -> None:
    """Show a service's endpoints or one endpoint's request schema. Free."""
    _echo(_run(lambda client: client.describe(service_id, method=method, path=path)))


@app.command()
def quote(
    plan_path: str = typer.Argument(..., help="Plan JSON file, or - for stdin."),
) -> None:
    """Validate a plan and price every node without paying. Free."""
    _echo(_run(lambda client: client.quote(_load_plan(plan_path))))


@app.command()
def submit(
    plan_path: str = typer.Argument(..., help="Plan JSON file, or - for stdin."),
    max_spend: float = typer.Option(
        ..., "--max-spend", min=0, help="Maximum USD to pay; refuse if the quoted total is higher."
    ),
    idempotency_key: str = typer.Option(
        ...,
        "--idempotency-key",
        help="New key per job; reuse it only to retry a submit whose response was lost.",
    ),
) -> None:
    """Quote a plan and pay for it if the total is within --max-spend."""
    _echo(
        _run(
            lambda client: client.submit(
                _load_plan(plan_path), max_spend=max_spend, idempotency_key=idempotency_key
            )
        )
    )


@app.command("job")
def job_status(
    job_id: str = typer.Argument(..., help="Job ID returned by `mercator submit`."),
) -> None:
    """Fetch a job's status and results. Free."""
    _echo(_run(lambda client: client.get_job(job_id)))


if __name__ == "__main__":
    app()
