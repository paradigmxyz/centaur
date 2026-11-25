"""CLI for websearch tool."""

from __future__ import annotations

import json
import os
import sys
from typing import Annotated

import typer
from dotenv import load_dotenv

from .client import _client

load_dotenv()

app = typer.Typer(name="websearch", help="Web search via Parallel, Exa, or Tako")


def _write(text: str) -> None:
    try:
        sys.stdout.write(text)
        sys.stdout.flush()
    except BrokenPipeError:
        # The reader (e.g. `head`) stopped early, which is success. Point stdout at
        # devnull so the interpreter's exit-time flush cannot fail with status 120.
        os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())


def _write_json(payload: dict) -> None:
    _write(json.dumps(payload, indent=2, ensure_ascii=False, default=str) + "\n")


@app.command("health")
def health(
    timeout_seconds: Annotated[float, typer.Option(help="Request timeout")] = 30.0,
):
    """Assert websearch connectivity and auth with a safe read-only check."""
    try:
        details = _client().search("health check", num_results=1, timeout_seconds=timeout_seconds)
    except Exception as exc:
        _write_json({"ok": False, "tool": "websearch", "error": str(exc), "details": {}})
        raise typer.Exit(1) from exc
    _write_json({"ok": True, "tool": "websearch", "error": None, "details": details})


@app.command()
def search(
    query: Annotated[str, typer.Argument(help="Search query or question.")],
    num_results: Annotated[int, typer.Option("--num-results", "-n", help="Maximum results")] = 10,
    include_domains: Annotated[
        list[str] | None,
        typer.Option("--include-domain", help="Restrict to this domain. Repeatable."),
    ] = None,
    exclude_domains: Annotated[
        list[str] | None, typer.Option("--exclude-domain", help="Exclude this domain. Repeatable.")
    ] = None,
    max_age_hours: Annotated[
        int | None, typer.Option(help="Only recent results (date-granular on Parallel and Tako)")
    ] = None,
    mode: Annotated[str | None, typer.Option(help="basic (faster) | advanced")] = None,
    max_chars_total: Annotated[
        int | None, typer.Option(help="Upper bound on total snippet characters")
    ] = None,
    session_id: Annotated[
        str | None, typer.Option(help="Stable ID reused across related searches (Parallel only)")
    ] = None,
    timeout_seconds: Annotated[float, typer.Option(help="Request timeout")] = 60.0,
    backend: Annotated[
        str | None,
        typer.Option(help="parallel | exa | tako (default: $WEBSEARCH_BACKEND, then parallel)"),
    ] = None,
    pretty: Annotated[bool, typer.Option(help="Print concise human-readable output")] = False,
    # Retired flags, accepted so older invocations keep working; reported in meta.warnings.
    synthesize: Annotated[
        bool | None, typer.Option("--synthesize/--no-synthesize", hidden=True)
    ] = None,
    client_model: Annotated[str | None, typer.Option(hidden=True)] = None,
    max_report_chars: Annotated[int | None, typer.Option(hidden=True)] = None,
    search_type: Annotated[str | None, typer.Option(hidden=True)] = None,
):
    """Search the web. Prints JSON with results and meta."""
    try:
        payload = _client().search(
            query,
            num_results=num_results,
            include_domains=include_domains,
            exclude_domains=exclude_domains,
            max_age_hours=max_age_hours,
            mode=mode,
            max_chars_total=max_chars_total,
            session_id=session_id,
            timeout_seconds=timeout_seconds,
            backend=backend,
            synthesize=synthesize,
            client_model=client_model,
            max_report_chars=max_report_chars,
            search_type=search_type,
        )
    except Exception as exc:
        print(f"search failed: {exc}", file=sys.stderr)
        raise typer.Exit(1) from exc

    if not pretty:
        _write_json(payload)
        return
    for warning in payload["meta"]["warnings"]:
        print(f"note: {warning}", file=sys.stderr)
    lines = [f"[{payload['meta']['backend']}] {payload['query']}"]
    for index, result in enumerate(payload["results"], 1):
        date = f" ({result['published_date']})" if result["published_date"] else ""
        lines.append(f"{index}. {result['title']}{date}\n   {result['url']}")
    if attribution := payload["meta"].get("attribution"):
        lines.append(attribution)
    _write("\n".join(lines) + "\n")


if __name__ == "__main__":
    app()
