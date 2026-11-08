"""CLI for Laminar trace investigation for Centaur agent executions."""

import json
from collections.abc import Callable
from typing import Any

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(name="laminar", help="Laminar trace investigation for Centaur agent executions")


def _print(payload: Any) -> None:
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


def _run(fn: Callable[[Any], Any]) -> None:
    """Run ``fn(client)``, print its JSON result, and exit non-zero on failure."""
    from .client import _client

    client = _client()
    try:
        result = fn(client)
    except Exception as exc:
        _print({"ok": False, "error": str(exc)})
        raise typer.Exit(1) from exc
    finally:
        client.close()
    _print(result)


@app.callback()
def main() -> None:
    """laminar CLI."""


@app.command("health")
def health():
    """Assert laminar connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.health()
        if isinstance(details, dict) and not details.get("ready", False):
            raise RuntimeError(str(details.get("response") or "laminar health check failed"))
        payload = {"ok": True, "tool": "laminar", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "laminar", "error": str(exc), "details": {}}
        _print(payload)
        raise typer.Exit(1) from exc
    finally:
        client.close()
    _print(payload)


@app.command("query")
def query(
    sql: str = typer.Argument(..., help="Read-only Laminar SQL (tables: traces, spans)"),
    params: str = typer.Option(
        None, "--params", "-p", help='JSON object of query parameters, e.g. \'{"id": "..."}\''
    ),
):
    """Run a read-only Laminar SQL query."""
    try:
        parameters = json.loads(params) if params else None
    except json.JSONDecodeError as exc:
        raise typer.BadParameter(f"--params is not valid JSON: {exc}") from exc
    if parameters is not None and not isinstance(parameters, dict):
        raise typer.BadParameter("--params must be a JSON object")
    _run(lambda c: c.query(sql, parameters))


@app.command("recent")
def recent(
    minutes: int = typer.Option(60, "--minutes", "-m", help="Look-back window in minutes"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max traces"),
):
    """List recent traces with duration, status, and top span name."""
    _run(lambda c: c.recent_traces(minutes=minutes, limit=limit))


@app.command("find")
def find(
    thread_key: str = typer.Option(None, "--thread", "-t", help="Centaur thread key"),
    execution_id: str = typer.Option(None, "--execution", "-e", help="Centaur execution id"),
    session_id: str = typer.Option(None, "--session", "-s", help="Laminar session id"),
    minutes: int = typer.Option(60 * 24, "--minutes", "-m", help="Look-back window in minutes"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max traces"),
):
    """Find traces by thread key, execution id, or session id."""
    if not any((thread_key, execution_id, session_id)):
        raise typer.BadParameter("Provide at least one of --thread, --execution, or --session")
    _run(
        lambda c: c.find_traces(
            thread_key=thread_key,
            execution_id=execution_id,
            session_id=session_id,
            minutes=minutes,
            limit=limit,
        )
    )


@app.command("trace")
def trace(
    trace_id: str = typer.Argument(..., help="Trace id (UUID)"),
    include_io: bool = typer.Option(False, "--io", help="Include span input/output"),
    io_chars: int = typer.Option(1000, "--io-chars", help="Truncate input/output to N chars"),
    limit: int = typer.Option(300, "--limit", "-n", help="Max spans"),
):
    """Show span-level detail for one trace."""
    _run(
        lambda c: c.get_trace(
            trace_id, include_io=include_io, input_output_chars=io_chars, limit=limit
        )
    )


@app.command("diagnose")
def diagnose(
    thread_key: str = typer.Argument(..., help="Centaur thread key"),
    minutes: int = typer.Option(60 * 24, "--minutes", "-m", help="Look-back window in minutes"),
    trace_limit: int = typer.Option(10, "--trace-limit", help="Max traces"),
    slow_span_limit: int = typer.Option(20, "--slow-span-limit", help="Max slow spans"),
):
    """Summarize traces, errors, and slowest spans for one Centaur thread."""
    _run(
        lambda c: c.diagnose_thread(
            thread_key,
            minutes=minutes,
            trace_limit=trace_limit,
            slow_span_limit=slow_span_limit,
        )
    )


@app.command("errors")
def errors(
    minutes: int = typer.Option(60, "--minutes", "-m", help="Look-back window in minutes"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max spans"),
):
    """List recent failed/error spans across the project."""
    _run(lambda c: c.errors(minutes=minutes, limit=limit))


if __name__ == "__main__":
    app()
