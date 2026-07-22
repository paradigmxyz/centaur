"""CLI for centaur-console sandbox permission introspection."""

from __future__ import annotations

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

load_dotenv()

app = typer.Typer(
    name="centaur-console",
    help="Inspect the current sandbox's centaur-console permissions",
)
console = Console()


def get_client(
    url: str | None = None,
    bearer_token: str | None = None,
):
    from .client import ConsoleClient

    return ConsoleClient(url=url, bearer_token=bearer_token)


@app.command("permissions")
def permissions(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Print the current sandbox's redacted permissions as JSON."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.sandbox_permissions()
    console.print_json(json.dumps(result, default=str))


@app.command("oauth-apps")
def oauth_apps(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Print enabled OAuth apps and their consent start URLs as JSON."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.sandbox_oauth_apps()
    console.print_json(json.dumps({"data": result}, default=str))


@app.command()
def health(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Assert the sandbox permissions endpoint is reachable and authorized."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        payload = client.health()
    print(json.dumps(payload, indent=2, default=str))
    if not payload.get("ok"):
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
