"""CLI for Market data via MPP (Machine Payments Protocol)."""

import json
from collections.abc import Callable

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(
    name="mpp",
    help="Market data and live service discovery via MPP (Machine Payments Protocol).",
)
services_app = typer.Typer(help="Discover public MPP services without making a payment.")
app.add_typer(services_app, name="services")


def _print_json(payload: object) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


def _run_discovery(operation: Callable[[], object]) -> None:
    try:
        _print_json(operation())
    except (RuntimeError, ValueError) as exc:
        _print_json({"error": str(exc)})
        raise typer.Exit(1) from exc


@app.callback()
def main() -> None:
    """mpp CLI."""


@app.command("health")
def health():
    """Assert mpp connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_trending()
        payload = {"ok": True, "tool": "mpp", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "mpp", "error": str(exc), "details": {}}
        _print_json(payload)
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    _print_json(payload)


@services_app.command("list")
def list_services(
    query: str | None = typer.Option(
        None, "--query", "-q", help="Text to match in catalog metadata"
    ),
    category: str | None = typer.Option(None, help="Exact service category"),
    tag: str | None = typer.Option(None, help="Exact service tag"),
    limit: int = typer.Option(20, min=1, max=100, help="Maximum services to return"),
) -> None:
    """List public MPP services with optional catalog filters."""
    from .client import _client

    client = _client()
    _run_discovery(
        lambda: {
            "services": client.list_services(query=query, category=category, tag=tag, limit=limit),
            "filters": {"query": query, "category": category, "tag": tag, "limit": limit},
        }
    )


@services_app.command("search")
def search_services(
    query: str = typer.Argument(..., help="Text to match in catalog metadata"),
    category: str | None = typer.Option(None, help="Exact service category"),
    tag: str | None = typer.Option(None, help="Exact service tag"),
    limit: int = typer.Option(20, min=1, max=100, help="Maximum services to return"),
) -> None:
    """Search public MPP services by id, name, description, category, or tag."""
    from .client import _client

    client = _client()
    _run_discovery(
        lambda: {
            "services": client.search_services(
                query=query, category=category, tag=tag, limit=limit
            ),
            "filters": {"query": query, "category": category, "tag": tag, "limit": limit},
        }
    )


@services_app.command("show")
def show_service(
    service: str = typer.Argument(..., help="Exact service id or unambiguous service name"),
) -> None:
    """Show one complete public MPP service record."""
    from .client import _client

    client = _client()
    _run_discovery(lambda: client.get_service(service))


if __name__ == "__main__":
    app()
