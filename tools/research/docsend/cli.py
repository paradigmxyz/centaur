"""CLI for DocSend document downloader."""

from dotenv import load_dotenv

load_dotenv()

import base64  # noqa: E402
import json  # noqa: E402
from pathlib import Path  # noqa: E402

import typer  # noqa: E402

app = typer.Typer(name="docsend", help="DocSend downloader — Browserbase + Playwright")


@app.callback()
def main() -> None:
    """docsend CLI."""


def get_client():
    from .client import DocsendClient

    return DocsendClient()


def _print_result(payload: dict) -> None:
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


def _handle_result(
    result: dict,
    output: str | None = None,
) -> None:
    payload = {key: value for key, value in result.items() if key != "data"}

    if result.get("status") not in {"ok", "deal_room_ready", "closed"}:
        _print_result(payload)
        raise typer.Exit(1)

    encoded_data = result.get("data")
    if encoded_data is None:
        _print_result(payload)
        return
    if not isinstance(encoded_data, str):
        _print_result({"status": "error", "error": "Download returned invalid file data"})
        raise typer.Exit(1)

    default_filename = Path(str(result.get("filename") or "docsend_document.pdf")).name
    output_path = Path(output or default_filename)
    try:
        output_path.write_bytes(base64.b64decode(encoded_data, validate=True))
    except Exception as exc:
        _print_result({"status": "error", "error": str(exc), "output": str(output_path)})
        raise typer.Exit(1) from exc
    payload["output"] = str(output_path)
    _print_result(payload)


@app.command("download")
def download(
    url: str = typer.Argument(..., help="DocSend document URL"),
    email: str = typer.Option(
        "", "--email", "-e", envvar="DOCSEND_EMAIL", help="Email for gated documents"
    ),
    passcode: str | None = typer.Option(
        None,
        "--passcode",
        envvar="DOCSEND_PASSCODE",
        help="Passcode for protected documents",
    ),
    output: str | None = typer.Option(None, "--output", "-o", help="Output PDF path"),
    session_timeout: int = typer.Option(
        600,
        "--session-timeout",
        min=60,
        max=1800,
        help="Browserbase session lifetime in seconds",
    ),
) -> None:
    """Download a DocSend document as a PDF."""
    result = get_client().download(
        url=url,
        email=email,
        passcode=passcode,
        session_timeout=session_timeout,
    )
    _handle_result(result, output)


@app.command("login")
def login(
    url: str = typer.Argument(..., help="DocSend Space URL"),
    email: str = typer.Option(
        "", "--email", "-e", envvar="DOCSEND_EMAIL", help="Email for the Space access gate"
    ),
    passcode: str | None = typer.Option(
        None,
        "--passcode",
        envvar="DOCSEND_PASSCODE",
        help="Passcode for a protected Space",
    ),
    session_timeout: int = typer.Option(
        600,
        "--session-timeout",
        min=60,
        max=1800,
        help="Browserbase session lifetime in seconds",
    ),
) -> None:
    """Authenticate and open a DocSend Space."""
    result = get_client().login(
        url=url,
        email=email,
        passcode=passcode,
        session_timeout=session_timeout,
    )
    _handle_result(result)


@app.command("resume")
def resume(
    session_id: str = typer.Argument(..., help="Resumable Browserbase session ID"),
    verification_link: str = typer.Option(
        ...,
        "--verification-link",
        envvar="DOCSEND_VERIFICATION_LINK",
        help="Verification URL sent by DocSend",
    ),
    output: str | None = typer.Option(None, "--output", "-o", help="Output PDF path"),
) -> None:
    """Resume a DocSend download after email verification."""
    result = get_client().resume(
        session_id=session_id,
        verification_link=verification_link,
    )
    _handle_result(result, output)


@app.command("list")
def list_space(
    session_id: str = typer.Argument(..., help="Active DocSend Browserbase session ID"),
) -> None:
    """List the root of a verified DocSend Space."""
    _handle_result(get_client().list_space(session_id=session_id))


@app.command("open-folder")
def open_folder(
    session_id: str = typer.Argument(..., help="Active DocSend Browserbase session ID"),
    folder_id: str = typer.Option(
        ...,
        "--folder-id",
        help="Folder ID returned by docsend list or open-folder",
    ),
) -> None:
    """Open one folder and list its immediate children."""
    _handle_result(get_client().open_folder(session_id=session_id, folder_id=folder_id))


@app.command("fetch")
def fetch(
    session_id: str = typer.Argument(..., help="Active DocSend Browserbase session ID"),
    item_id: str = typer.Option(
        ...,
        "--item-id",
        help="Item ID returned by docsend list or open-folder",
    ),
    output: str | None = typer.Option(None, "--output", "-o", help="Output file path"),
) -> None:
    """Download one permitted item from a verified DocSend Space."""
    _handle_result(
        get_client().fetch(session_id=session_id, item_id=item_id),
        output,
    )


@app.command("close")
def close_session(
    session_id: str = typer.Argument(..., help="DocSend Browserbase session ID"),
) -> None:
    """Release a resumable DocSend Browserbase session."""
    _handle_result(get_client().close_session(session_id=session_id))


@app.command("health")
def health() -> None:
    """Assert docsend connectivity and auth with a safe read-only check."""
    try:
        get_client()
        details = {"auth_mode": "local-only", "live_probe": False}
        payload = {"ok": True, "tool": "docsend", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "docsend", "error": str(exc), "details": {}}
        _print_result(payload)
        raise typer.Exit(1) from exc
    _print_result(payload)


if __name__ == "__main__":
    app()
