"""Command-line interface for Browser Use."""

from __future__ import annotations

import json

import httpx
import typer

from .client import DEFAULT_MODEL, BrowserUseError, _client, output_text

app = typer.Typer(
    name="browser-use",
    help="Browser Use cloud browsers, LLM gateway and web search. Page control (HTML, click, ...) "
    "is in the Code Mode browser_use connector.",
)
browser_app = typer.Typer(help="Start, inspect and stop cloud browsers.")
app.add_typer(browser_app, name="browser")


def _echo(payload: dict) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))


def _error_message(exc: Exception) -> str:
    if isinstance(exc, httpx.HTTPError):
        return f"Browser Use request failed ({type(exc).__name__})."
    return str(exc)


def _run(operation) -> dict:
    try:
        with _client() as client:
            return operation(client)
    except (httpx.HTTPError, BrowserUseError, ValueError) as exc:
        typer.echo(_error_message(exc), err=True)
        raise typer.Exit(1) from None


@app.command()
def health() -> None:
    """Check Browser Use authentication and connectivity by listing LLM models (free)."""
    try:
        with _client() as client:
            details = client.list_models()
    except (httpx.HTTPError, BrowserUseError, ValueError) as exc:
        _echo({"ok": False, "tool": "browser-use", "error": _error_message(exc), "details": {}})
        raise typer.Exit(1) from None
    _echo({"ok": True, "tool": "browser-use", "error": None, "details": details})


@app.command()
def models() -> None:
    """List the LLM gateway's models."""
    _echo(_run(lambda client: client.list_models()))


@app.command()
def respond(
    input: str = typer.Argument(..., help="Prompt text."),
    model: str = typer.Option(DEFAULT_MODEL, help="Gateway model, see `browser-use models`."),
    instructions: str | None = typer.Option(None, help="System instructions."),
    max_output_tokens: int | None = typer.Option(None, help="Cap on output tokens."),
    reasoning_effort: str | None = typer.Option(None, help="minimal, low, medium or high."),
    raw: bool = typer.Option(False, "--raw", help="Print the whole Responses API response."),
) -> None:
    """Ask a Browser Use gateway model (OpenAI Responses API)."""
    response = _run(
        lambda client: client.respond(
            input,
            model=model,
            instructions=instructions,
            max_output_tokens=max_output_tokens,
            reasoning_effort=reasoning_effort,
        )
    )
    if raw:
        _echo(response)
    else:
        _echo(
            {
                "output_text": output_text(response),
                "usage": response.get("usage"),
                "browser_use": response.get("browser_use"),
            }
        )


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query."),
    num_results: int | None = typer.Option(None, min=1, max=20, help="Results (default 8)."),
    category: str | None = typer.Option(None, help="company, people, news or publication."),
) -> None:
    """Search the web with Browser Use."""
    _echo(_run(lambda client: client.search(query, num_results=num_results, category=category)))


@browser_app.command("create")
def browser_create(
    timeout_minutes: int = typer.Option(
        15, min=1, max=240, help="Minutes before the browser stops."
    ),
    profile_id: str | None = typer.Option(None, help="Stored Browser Use profile."),
    proxy_country_code: str | None = typer.Option(None, help="Proxy exit country (default us)."),
    network_policy_id: str | None = typer.Option(None, help="Browser Use network policy."),
) -> None:
    """Start a cloud browser. It bills until `browser stop` or its timeout."""
    _echo(
        _run(
            lambda client: client.create_browser(
                timeout_minutes=timeout_minutes,
                profile_id=profile_id,
                proxy_country_code=proxy_country_code,
                network_policy_id=network_policy_id,
            )
        )
    )


@browser_app.command("get")
def browser_get(browser_id: str = typer.Argument(..., help="Browser ID.")) -> None:
    """Show a browser's state and costs."""
    _echo(_run(lambda client: client.get_browser(browser_id)))


@browser_app.command("stop")
def browser_stop(browser_id: str = typer.Argument(..., help="Browser ID.")) -> None:
    """Stop a browser."""
    _echo(_run(lambda client: client.stop_browser(browser_id)))


if __name__ == "__main__":
    app()
