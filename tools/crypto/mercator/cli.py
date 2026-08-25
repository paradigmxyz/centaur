"""Command-line wrapper for the Centaur Mercator tool."""

from __future__ import annotations

import json
from typing import Any

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="mercator",
    help="Discover, quote, and execute Mercator services through Centaur",
    no_args_is_help=True,
)


def _json_object(value: str, label: str) -> dict[str, Any]:
    try:
        parsed = json.loads(value)
    except ValueError as exc:
        raise typer.BadParameter(f"{label} must be valid JSON") from exc
    if not isinstance(parsed, dict):
        raise typer.BadParameter(f"{label} must be a JSON object")
    return parsed


def _print(value: dict[str, Any]) -> None:
    print(json.dumps(value, indent=2, default=str))


@app.command("search")
def search(query: str, limit: int = typer.Option(8, min=1, max=20)) -> None:
    """Search Mercator services for a complete outcome."""
    from .client import _client

    _print(_client().search_services(query, limit=limit))


@app.command("describe")
def describe(service_id: str, method: str | None = None, path: str | None = None) -> None:
    """Describe a Mercator service or endpoint."""
    from .client import _client

    _print(_client().describe_service(service_id, method=method, path=path))


@app.command("quote")
def quote(plan: str = typer.Option(..., help="Mercator plan JSON")) -> None:
    """Quote a Mercator plan without paying."""
    from .client import _client

    _print(_client().quote_plan(_json_object(plan, "plan")))


@app.command("create-job")
def create_job(
    plan: str = typer.Option(..., help="Unchanged quoted plan JSON"),
    idempotency_key: str = typer.Option(..., min=8, max=200),
) -> None:
    """Create the canonical paid-job handoff without executing payment."""
    from .client import _client

    _print(_client().create_job(_json_object(plan, "plan"), idempotency_key))


@app.command("submit-job")
def submit_job(
    handoff: str = typer.Option(..., help="Handoff JSON returned by create-job"),
    approved: bool = typer.Option(False, help="User accepted the displayed quote"),
) -> None:
    """Submit an approved handoff through Centaur's trusted payer."""
    from .client import _client

    _print(_client().submit_job(_json_object(handoff, "handoff"), approved=approved))


@app.command("get-job")
def get_job(job_id: str) -> None:
    """Read or poll an existing Mercator job."""
    from .client import _client

    _print(_client().get_job(job_id))
