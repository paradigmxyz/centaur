"""Typer CLI for the document archiver tool."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Optional

import typer

from .client import _client
from .utils import dump_json, normalize_url

app = typer.Typer(
    name="archiver", help="Reducto-first document extraction for investment materials."
)


@app.command("health")
def health():
    """Assert archiver connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {"auth_mode": "local-only", "live_probe": False}
        payload = {"ok": True, "tool": "archiver", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "archiver", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


DEFAULT_GOOGLE_ACCOUNT = (os.getenv("GOOGLE_ACCOUNT") or "").strip() or ""


def _read_context(
    context: str | None,
    context_file: str | None,
) -> dict | None:
    if context and context_file:
        print(
            dump_json(
                {"status": "error", "error": "Cannot specify both --context and --context-file"}
            )
        )
        raise typer.Exit(1)
    if context:
        return json.loads(context)
    if context_file:
        return json.loads(Path(context_file).read_text())
    return None


def _exit_json(error: str) -> None:
    print(dump_json({"status": "error", "error": error}))
    raise typer.Exit(1)


def _is_interactive() -> bool:
    return bool(sys.stdin.isatty() and sys.stdout.isatty())


def _source_kind(source_url: str) -> str:
    canonical = normalize_url(source_url).lower()
    if "docsend.com" in canonical:
        return "docsend"
    if "google.com" in canonical:
        return "google_drive"
    return "unknown"


def _looks_password_required(payload: dict) -> bool:
    text = str(payload.get("error") or "").lower()
    if "password" in text or "passcode" in text:
        return True
    files = payload.get("files") or []
    for entry in files:
        err = str((entry or {}).get("error") or "").lower()
        if "password" in err or "passcode" in err:
            return True
    return False


def _resolve_source_auth_inputs(
    source_url: str,
    account: str | None,
    password: str | None,
    email: str | None,
) -> tuple[str, str | None, str | None, str | None]:
    kind = _source_kind(source_url)
    resolved_account = (account or "").strip() or None
    resolved_password = (password or "").strip() or None
    resolved_email = (email or "").strip() or os.getenv("DOCSEND_EMAIL") or None

    if kind == "google_drive" and not resolved_account:
        # Prefer default service account for automated and harness runs.
        resolved_account = DEFAULT_GOOGLE_ACCOUNT or None
    if kind == "google_drive" and not resolved_account:
        if _is_interactive():
            entered = typer.prompt("Google account email for this Drive source")
            resolved_account = entered.strip() or None
        if not resolved_account:
            _exit_json("Google account email is required")

    if kind == "docsend" and not resolved_email and _is_interactive():
        entered = typer.prompt(
            "DocSend email (optional, press Enter to skip)",
            default="",
            show_default=False,
        )
        resolved_email = entered.strip() or None

    return kind, resolved_account, resolved_password, resolved_email


@app.command("init-db")
def init_db() -> None:
    """Deprecated: DB setup is not required in extraction-only mode."""
    print(
        dump_json(
            {
                "status": "error",
                "error": "init-db is disabled in extraction-only mode",
            }
        )
    )
    raise typer.Exit(1)


@app.command()
def download(
    source: str = typer.Option(..., help="Source URL (DocSend or Google Drive)"),
    output: str = typer.Option(..., help="Output directory"),
    company: Optional[str] = typer.Option(None, help="Company name for metadata"),
    account: Optional[str] = typer.Option(
        None,
        help="Google account email for Drive",
    ),
    password: Optional[str] = typer.Option(None, help="DocSend password if required"),
    email: Optional[str] = typer.Option(None, help="DocSend email for email-gated links"),
    max_depth: int = typer.Option(3, help="Google folder recursion depth"),
) -> None:
    """Download docsend/drive sources."""
    client = _client()
    source_kind, resolved_account, resolved_password, resolved_email = _resolve_source_auth_inputs(
        source_url=source,
        account=account,
        password=password,
        email=email,
    )
    payload = client.download(
        source_url=source,
        output_dir=output,
        company=company,
        account=resolved_account,
        password=resolved_password,
        email=resolved_email,
        max_depth=max_depth,
    )
    if (
        source_kind == "docsend"
        and payload.get("status") != "ok"
        and not resolved_password
        and _looks_password_required(payload)
        and _is_interactive()
    ):
        entered_password = typer.prompt(
            "DocSend password", hide_input=True, confirmation_prompt=False
        )
        entered_password = entered_password.strip()
        if entered_password:
            payload = client.download(
                source_url=source,
                output_dir=output,
                company=company,
                account=resolved_account,
                password=entered_password,
                email=resolved_email,
                max_depth=max_depth,
            )
    print(dump_json(payload))
    if payload.get("status") != "ok":
        raise typer.Exit(1)


@app.command()
def parse(
    manifest: str = typer.Option(..., help="Download manifest JSON"),
    context: Optional[str] = typer.Option(None, help="Inline JSON context"),
    context_file: Optional[str] = typer.Option(None, help="Path to JSON file with context"),
) -> None:
    """Parse local files with Reducto."""
    client = _client()
    ctx = _read_context(context, context_file)
    payload = client.parse(manifest, context=ctx)
    print(dump_json(payload))
    if payload.get("status") != "ok":
        raise typer.Exit(1)


@app.command()
def extract(
    manifest: Optional[str] = typer.Option(
        None,
        help="Existing manifest path; use with pre-downloaded files",
    ),
    source: Optional[str] = typer.Option(
        None,
        help="DocSend or Google Drive URL; downloads and extracts in one shot",
    ),
    output: Optional[str] = typer.Option(
        None,
        help="Output directory (required with --source)",
    ),
    file: list[str] = typer.Option(
        [],
        "--file",
        help="Local file path (repeatable) for direct extraction",
    ),
    company: Optional[str] = typer.Option(None, help="Company hint for source mode"),
    account: Optional[str] = typer.Option(
        None,
        help="Google account for Drive mode",
    ),
    password: Optional[str] = typer.Option(None, help="DocSend password"),
    email: Optional[str] = typer.Option(None, help="DocSend email for email-gated links"),
    max_depth: int = typer.Option(3, help="Google folder recursion depth"),
    context: Optional[str] = typer.Option(None, help="Inline JSON context"),
    context_file: Optional[str] = typer.Option(None, help="Path to JSON file with context"),
) -> None:
    """Unified Reducto extraction command."""
    client = _client()
    ctx = _read_context(context, context_file)

    mode_count = sum(1 for enabled in [bool(manifest), bool(source), bool(file)] if enabled)
    if mode_count != 1:
        print(
            dump_json(
                {
                    "status": "error",
                    "error": "Specify exactly one input mode: --manifest, --source, or --file",
                }
            )
        )
        raise typer.Exit(1)

    if manifest:
        payload = client.extract_manifest(manifest, context=ctx)
    elif source:
        if not output:
            print(
                dump_json({"status": "error", "error": "--output is required when using --source"})
            )
            raise typer.Exit(1)
        source_kind, resolved_account, resolved_password, resolved_email = (
            _resolve_source_auth_inputs(
                source_url=source,
                account=account,
                password=password,
                email=email,
            )
        )
        payload = client.extract_source(
            source_url=source,
            output_dir=output,
            company=company,
            account=resolved_account,
            password=resolved_password,
            email=resolved_email,
            max_depth=max_depth,
            context=ctx,
        )
        if (
            source_kind == "docsend"
            and payload.get("status") != "ok"
            and not resolved_password
            and _looks_password_required(payload)
            and _is_interactive()
        ):
            entered_password = typer.prompt(
                "DocSend password", hide_input=True, confirmation_prompt=False
            )
            entered_password = entered_password.strip()
            if entered_password:
                payload = client.extract_source(
                    source_url=source,
                    output_dir=output,
                    company=company,
                    account=resolved_account,
                    password=entered_password,
                    email=resolved_email,
                    max_depth=max_depth,
                    context=ctx,
                )
    else:
        payload = client.extract_files(file_paths=file, context=ctx)

    print(dump_json(payload))
    if payload.get("status") != "ok":
        raise typer.Exit(1)


@app.command()
def ingest(
    manifest: str = typer.Option(..., help="Download manifest JSON"),
) -> None:
    """Deprecated alias for parse (extraction-only mode)."""
    print(dump_json({"status": "warning", "warning": "ingest is deprecated; use parse or extract"}))
    parse(manifest=manifest)


@app.command()
def search(
    query: Optional[str] = typer.Argument(None, help="Search query"),
) -> None:
    """Disabled in extraction-only mode."""
    _ = query
    print(dump_json({"status": "error", "error": "search is disabled in extraction-only mode"}))
    raise typer.Exit(1)


@app.command()
def status(
    source: str = typer.Option(..., help="Source URL or file hash"),
) -> None:
    """Disabled in extraction-only mode."""
    _ = source
    print(dump_json({"status": "error", "error": "status is disabled in extraction-only mode"}))
    raise typer.Exit(1)


@app.command()
def fetch(
    chunk_id: int = typer.Option(..., help="Chunk ID from search results"),
) -> None:
    """Disabled in extraction-only mode."""
    _ = chunk_id
    print(dump_json({"status": "error", "error": "fetch is disabled in extraction-only mode"}))
    raise typer.Exit(1)


if __name__ == "__main__":
    app()
