"""Command-line interface for SEC EDGAR."""

from __future__ import annotations

import json
from typing import Any, NoReturn

import httpx
import typer

from .client import DEFAULT_MAX_CHARS, USER_AGENT_ENV, _client

app = typer.Typer(
    name="edgar",
    help=(
        "SEC EDGAR: company lookup, filings, full-text search, filing documents, and XBRL "
        f"financial data. No API key needed; set {USER_AGENT_ENV} to override the "
        "User-Agent sent to SEC."
    ),
    no_args_is_help=True,
)

_ERRORS = (httpx.HTTPError, LookupError, ValueError, RuntimeError)


def _echo(payload: Any) -> None:
    typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))


def _error_message(exc: Exception) -> str:
    if isinstance(exc, httpx.HTTPStatusError):
        status = exc.response.status_code
        message = f"EDGAR API error: HTTP {status}"
        if status == 403:
            message += (
                f" (SEC may be rate limiting or rejecting the User-Agent; see {USER_AGENT_ENV})"
            )
        elif status == 404:
            message += " (not found)"
        return message
    if isinstance(exc, httpx.HTTPError):
        return f"EDGAR request failed ({type(exc).__name__})."
    return str(exc)


def _fail(exc: Exception) -> NoReturn:
    typer.echo(_error_message(exc), err=True)
    raise typer.Exit(1) from None


def _forms(value: str | None) -> list[str] | None:
    return [form.strip() for form in value.split(",") if form.strip()] if value else None


@app.command()
def health() -> None:
    """Check connectivity to EDGAR."""
    try:
        with _client() as client:
            data = client.submissions("320193")
    except _ERRORS as exc:
        _echo({"ok": False, "tool": "edgar", "error": _error_message(exc), "details": {}})
        raise typer.Exit(1) from None
    _echo(
        {
            "ok": True,
            "tool": "edgar",
            "error": None,
            "details": {"sample_company": data.get("name"), "user_agent": client.user_agent},
        }
    )


@app.command()
def lookup(
    query: str = typer.Argument(..., help="Ticker, CIK, or company name fragment."),
    limit: int = typer.Option(10, "--limit", "-n", help="Maximum results."),
) -> None:
    """Find a company's CIK by ticker or name."""
    try:
        with _client() as client:
            _echo(client.lookup(query, limit=limit))
    except _ERRORS as exc:
        _fail(exc)


@app.command()
def filings(
    company: str = typer.Argument(..., help="Ticker or CIK."),
    form: str | None = typer.Option(None, "--form", "-f", help="Form type(s), comma-separated."),
    since: str | None = typer.Option(None, "--since", help="Earliest filing date (YYYY-MM-DD)."),
    until: str | None = typer.Option(None, "--until", help="Latest filing date (YYYY-MM-DD)."),
    limit: int = typer.Option(20, "--limit", "-n", help="Maximum filings."),
) -> None:
    """List a company's filings, newest first."""
    try:
        with _client() as client:
            _echo(
                client.filings(company, forms=_forms(form), since=since, until=until, limit=limit)
            )
    except _ERRORS as exc:
        _fail(exc)


@app.command()
def search(
    query: str = typer.Argument(
        ..., help="Full-text query; quote phrases, e.g. '\"supply chain\"'."
    ),
    form: str | None = typer.Option(None, "--form", "-f", help="Form type(s), comma-separated."),
    start: str | None = typer.Option(None, "--from", help="Earliest filing date (YYYY-MM-DD)."),
    end: str | None = typer.Option(None, "--to", help="Latest filing date (YYYY-MM-DD)."),
    company: str | None = typer.Option(None, "--company", "-c", help="Restrict to a ticker/CIK."),
    limit: int = typer.Option(20, "--limit", "-n", help="Maximum results."),
    offset: int = typer.Option(0, "--offset", help="Result offset for paging."),
) -> None:
    """Full-text search across EDGAR filings (2001 onward)."""
    try:
        with _client() as client:
            _echo(
                client.search(
                    query,
                    forms=_forms(form),
                    start=start,
                    end=end,
                    identifier=company,
                    limit=limit,
                    offset=offset,
                )
            )
    except _ERRORS as exc:
        _fail(exc)


@app.command()
def files(
    company: str = typer.Argument(..., help="Ticker or CIK of the filer."),
    accession: str = typer.Argument(..., help="Accession number, e.g. 0000320193-24-000123."),
) -> None:
    """List the documents in a filing."""
    try:
        with _client() as client:
            _echo(client.files(company, accession))
    except _ERRORS as exc:
        _fail(exc)


@app.command()
def get(
    company: str = typer.Argument(..., help="Ticker or CIK of the filer."),
    accession: str = typer.Argument(..., help="Accession number, e.g. 0000320193-24-000123."),
    filename: str | None = typer.Option(
        None, "--file", help="Document in the filing (default: primary document)."
    ),
    max_chars: int = typer.Option(DEFAULT_MAX_CHARS, "--max-chars", help="Characters to return."),
    offset: int = typer.Option(0, "--offset", help="Character offset for reading further."),
    raw: bool = typer.Option(False, "--raw", help="Return raw HTML instead of extracted text."),
    output: str | None = typer.Option(
        None, "--output", "-o", help="Write the full text to this file instead of stdout."
    ),
) -> None:
    """Fetch a filing document as text."""
    try:
        with _client() as client:
            if output:
                doc = client.document(
                    company, accession, filename=filename, max_chars=10**9, raw=raw
                )
                with open(output, "w", encoding="utf-8") as handle:
                    handle.write(doc.pop("text"))
                doc.update({"output": output, "truncated": False})
                _echo(doc)
                return
            _echo(
                client.document(
                    company,
                    accession,
                    filename=filename,
                    max_chars=max_chars,
                    offset=offset,
                    raw=raw,
                )
            )
    except (*_ERRORS, OSError) as exc:
        _fail(exc)


@app.command()
def concepts(
    company: str = typer.Argument(..., help="Ticker or CIK."),
    taxonomy: str | None = typer.Option(
        None, "--taxonomy", "-t", help="Filter by taxonomy, e.g. us-gaap or dei."
    ),
    grep: str | None = typer.Option(None, "--grep", "-g", help="Filter concepts by substring."),
) -> None:
    """List the XBRL concepts a company reports."""
    try:
        with _client() as client:
            result = client.concepts(company, taxonomy=taxonomy)
    except _ERRORS as exc:
        _fail(exc)
    if grep:
        needle = grep.lower()
        result["concepts"] = [
            c
            for c in result["concepts"]
            if needle in c["concept"].lower() or needle in str(c["label"] or "").lower()
        ]
    _echo(result)


@app.command()
def facts(
    company: str = typer.Argument(..., help="Ticker or CIK."),
    concept: str = typer.Argument(..., help="XBRL concept, e.g. us-gaap:Revenues."),
    form: str | None = typer.Option(None, "--form", "-f", help="Only facts from this form."),
    unit: str | None = typer.Option(None, "--unit", "-u", help="Only this unit, e.g. USD."),
    limit: int = typer.Option(20, "--limit", "-n", help="Latest values per unit."),
) -> None:
    """Show a company's reported values for one XBRL concept, newest first."""
    try:
        with _client() as client:
            _echo(client.facts(company, concept, form=form, unit=unit, limit=limit))
    except _ERRORS as exc:
        _fail(exc)


@app.command()
def frame(
    concept: str = typer.Argument(..., help="XBRL concept, e.g. us-gaap:Revenues."),
    period: str = typer.Argument(..., help="CY2023 (annual), CY2023Q4 (quarter), CY2023Q4I."),
    unit: str = typer.Option("USD", "--unit", "-u", help="Unit, e.g. USD, shares, USD-per-shares."),
    limit: int = typer.Option(20, "--limit", "-n", help="Maximum companies."),
    ascending: bool = typer.Option(False, "--ascending", help="Sort smallest first."),
) -> None:
    """Compare one concept across all companies for a period."""
    try:
        with _client() as client:
            _echo(client.frame(concept, period, unit=unit, limit=limit, ascending=ascending))
    except _ERRORS as exc:
        _fail(exc)


if __name__ == "__main__":
    app()
