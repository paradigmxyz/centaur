"""CLI for registry-backed Machine Payments Protocol services."""

import json
from collections.abc import Callable
from typing import Any

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="mpp",
    help="Discover and call registered MPP services with transparent proxy payments.",
)


def _print_json(payload: object) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


def _run(operation: Callable[[], object]) -> None:
    try:
        _print_json(operation())
    except (RuntimeError, ValueError) as exc:
        _print_json({"error": str(exc)})
        raise typer.Exit(1) from exc


def _json_object(value: str | None, label: str) -> dict[str, Any] | None:
    if value is None:
        return None
    try:
        payload = json.loads(value)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter(f"{label} must be valid JSON") from exc
    if not isinstance(payload, dict):
        raise typer.BadParameter(f"{label} must be a JSON object")
    return payload


def _json_value(value: str | None, label: str) -> Any:
    if value is None:
        return None
    try:
        return json.loads(value)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter(f"{label} must be valid JSON") from exc


@app.callback()
def main() -> None:
    """Discover and call registered MPP services."""


@app.command("list")
def list_services(
    query: str | None = typer.Option(
        None, "--query", "-q", help="Text to match in catalog metadata"
    ),
    category: str | None = typer.Option(None, help="Exact service category"),
    tag: str | None = typer.Option(None, help="Exact service tag"),
    limit: int = typer.Option(20, min=1, max=100, help="Maximum services to return"),
) -> None:
    """List registered services and their executable endpoint counts."""
    from .client import _client

    client = _client()
    _run(
        lambda: client.list_services(
            query=query,
            category=category,
            tag=tag,
            limit=limit,
        )
    )


@app.command("search")
def search_services(
    query: str = typer.Argument(..., help="Text to match in catalog metadata"),
    category: str | None = typer.Option(None, help="Exact service category"),
    tag: str | None = typer.Option(None, help="Exact service tag"),
    limit: int = typer.Option(20, min=1, max=100, help="Maximum services to return"),
) -> None:
    """Search registered services."""
    from .client import _client

    client = _client()
    _run(
        lambda: client.search_services(
            query=query,
            category=category,
            tag=tag,
            limit=limit,
        )
    )


@app.command("show")
def show_service(
    service: str = typer.Argument(..., help="Exact service id or unambiguous name"),
) -> None:
    """Show one service, endpoints, payment metadata, and availability."""
    from .client import _client

    client = _client()
    _run(lambda: client.show_service(service))


@app.command("request")
def request_service(
    service: str = typer.Argument(..., help="Exact registry service id"),
    method: str = typer.Option(..., "--method", "-X", help="Registered HTTP method"),
    path: str = typer.Option(..., "--path", help="Exact registered path template"),
    path_params: str | None = typer.Option(
        None, "--path-params", help="JSON object of path-template values"
    ),
    query: str | None = typer.Option(None, "--query", help="JSON object of query parameters"),
    body: str | None = typer.Option(None, "--body", help="JSON request body"),
) -> None:
    """Call one registered route through Centaur's transparent payment proxy."""
    from .client import _client

    parsed_path_params = _json_object(path_params, "--path-params")
    parsed_query = _json_object(query, "--query")
    parsed_body = _json_value(body, "--body")
    if parsed_body is not None and not isinstance(parsed_body, (dict, list)):
        raise typer.BadParameter("--body must be a JSON object or list")

    client = _client()
    _run(
        lambda: client.request(
            service=service,
            method=method,
            path=path,
            path_params=parsed_path_params,
            query=parsed_query,
            body=parsed_body,
        )
    )


@app.command("health")
def health() -> None:
    """Refresh the registry and report cache and policy readiness."""
    from .client import _client

    client = _client()
    _run(client.health)


if __name__ == "__main__":
    app()
