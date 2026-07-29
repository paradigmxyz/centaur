"""CLI for websearch tool."""

from __future__ import annotations

import asyncio
import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from .client import _client

load_dotenv()

app = typer.Typer(name="websearch", help="Web search and deep research via Parallel")


@app.command("health")
def health(
    timeout_seconds: float = typer.Option(30.0, "--timeout-seconds", help="Request timeout"),
):
    """Assert websearch connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = asyncio.run(
            client.search(
                "health check", num_results=1, timeout_seconds=timeout_seconds, synthesize=False
            )
        )
        payload = {"ok": True, "tool": "websearch", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "websearch", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console(stderr=True)


def _print_json(payload: dict) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True, default=str))


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query or research question."),
    num_results: int = typer.Option(10, "--num-results", "-n", help="Maximum results"),
    timeout_seconds: float = typer.Option(60.0, "--timeout-seconds", help="Request timeout"),
    synthesize: bool = typer.Option(
        True, "--synthesize/--no-synthesize", help="Compose a cited answer from excerpts"
    ),
    mode: str = typer.Option(None, "--mode", help="Search mode: basic | advanced (REST only)"),
    client_model: str = typer.Option(
        None,
        "--client-model",
        help="Identifier of the consuming LLM (e.g. claude-opus-4-7)",
    ),
    max_chars_total: int = typer.Option(
        None, "--max-chars-total", help="Upper bound on total excerpt characters (REST only)"
    ),
    include_domains: list[str] = typer.Option(
        None, "--include-domain", help="Restrict to these domains (REST only). Repeatable."
    ),
    exclude_domains: list[str] = typer.Option(
        None, "--exclude-domain", help="Exclude these domains (REST only). Repeatable."
    ),
    max_age_hours: int = typer.Option(
        None,
        "--max-age-hours",
        help=(
            "Recency filter (REST only). Rounded DOWN to a UTC calendar-date "
            "cutoff — Parallel's source policy is date-granular, not hour-precise."
        ),
    ),
    session_id: str = typer.Option(
        None,
        "--session-id",
        help="Stable session ID (UUID); reuse across related Search/Extract calls",
    ),
    max_report_chars: int = typer.Option(
        12000, "--max-report-chars", help="Maximum composed answer length in characters"
    ),
    pretty: bool = typer.Option(False, "--pretty", help="Print concise human-readable output"),
    # Backward-compat shim for the original Exa-backed tool's --search-type flag.
    # Hidden from --help; if passed, we warn and ignore (no Parallel equivalent).
    search_type: str = typer.Option(None, "--search-type", help="[deprecated] no-op", hidden=True),
):
    """Search the web via Parallel (free MCP or paid REST)."""
    client = _client()
    if search_type:
        console.print(
            f"[yellow]--search-type={search_type!r} is deprecated and ignored (no "
            "Parallel equivalent for Exa's neural/keyword/auto modes).[/]"
        )
    try:
        payload = asyncio.run(
            client.search(
                query=query,
                num_results=num_results,
                timeout_seconds=timeout_seconds,
                synthesize=synthesize,
                mode=mode,
                client_model=client_model,
                max_chars_total=max_chars_total,
                include_domains=include_domains or None,
                exclude_domains=exclude_domains or None,
                max_age_hours=max_age_hours,
                session_id=session_id,
                max_report_chars=max_report_chars,
            )
        )
    except Exception as exc:  # pragma: no cover - CLI surface
        console.print(f"[red]search failed:[/] {exc}")
        raise typer.Exit(1) from exc

    if pretty:
        out_console = Console()
        out_console.print(f"[bold]Query:[/] {payload['query']}")
        out_console.print(f"[dim]backend: {payload['meta'].get('backend')}[/]")
        attribution = payload["meta"].get("attribution")
        if attribution:
            out_console.print(f"[dim]{attribution}[/]")
        if payload["meta"].get("partial_failures"):
            for failure in payload["meta"]["partial_failures"]:
                out_console.print(f"[yellow]note: {failure.get('error')}[/]")
        if payload.get("answer_markdown"):
            out_console.print(payload["answer_markdown"])
        out_console.print(f"\n[bold]Results:[/] {len(payload['results'])}")
        for row in payload["results"][:10]:
            out_console.print(f"- {row['title']} ({row['url']})")
        return
    _print_json(payload)


@app.command("deep-research")
def deep_research_command(
    question: str = typer.Argument(..., help="Research question"),
    processor: str = typer.Option(
        None,
        "--processor",
        "-p",
        help="Task processor (pro/pro-fast/ultra/ultra-fast/ultra2x/ultra4x/ultra8x)",
    ),
    timeout_seconds: float = typer.Option(
        None,
        "--timeout-seconds",
        help="Request timeout. Defaults to a processor-appropriate value.",
    ),
    max_report_chars: int = typer.Option(
        50000, "--max-report-chars", help="Maximum report length in characters"
    ),
    pretty: bool = typer.Option(False, "--pretty", help="Print markdown report only"),
    # Backward-compat shims for the original Exa+Claude iterative pipeline.
    # Hidden from --help; if passed, we warn and ignore (single-call Task API
    # replaces planner→search→reviewer→writer loops).
    max_iterations: int = typer.Option(
        None, "--max-iterations", help="[deprecated] no-op", hidden=True
    ),
    num_queries_per_iteration: int = typer.Option(
        None, "--num-queries-per-iteration", help="[deprecated] no-op", hidden=True
    ),
    num_results_per_query: int = typer.Option(
        None, "--num-results-per-query", help="[deprecated] no-op", hidden=True
    ),
):
    """Run Parallel deep research (requires PARALLEL_API_KEY)."""
    client = _client()

    def _progress(stage: str) -> None:
        console.print(f"[dim]{stage}[/]")

    client._set_progress_callback(_progress)
    deprecated_passed = [
        name
        for name, value in (
            ("--max-iterations", max_iterations),
            ("--num-queries-per-iteration", num_queries_per_iteration),
            ("--num-results-per-query", num_results_per_query),
        )
        if value is not None
    ]
    if deprecated_passed:
        console.print(
            f"[yellow]Ignored deprecated flags: {', '.join(deprecated_passed)} "
            "(Parallel Task API is single-call; iteration knobs no longer apply).[/]"
        )
    try:
        payload = asyncio.run(
            client.deep_research(
                question=question,
                processor=processor,
                timeout_seconds=timeout_seconds,
                max_report_chars=max_report_chars,
            )
        )
    except Exception as exc:  # pragma: no cover - CLI surface
        console.print(f"[red]deep research failed:[/] {exc}")
        raise typer.Exit(1) from exc

    if pretty:
        print(payload["answer_markdown"])
        return
    _print_json(payload)


if __name__ == "__main__":
    app()
