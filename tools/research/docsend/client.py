"""Download DocSend documents and Space files through Browserbase."""

from __future__ import annotations

import asyncio
import base64
import concurrent.futures
import json
import logging
import mimetypes
import os
import re
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager, suppress
from functools import partial
from io import BytesIO
from urllib.parse import urljoin, urlparse

import httpx
from browserbase import APIStatusError, AsyncBrowserbase
from PIL import Image
from websockets.asyncio.client import connect as websocket_connect
from websockets.asyncio.server import serve as websocket_serve
from websockets.proxy import get_proxy as get_websocket_proxy
from websockets.uri import parse_uri as parse_websocket_uri

from centaur_sdk import secret

BROWSERBASE_PROXY_COUNTRY = os.environ.get("BROWSERBASE_PROXY_COUNTRY", "")  # noqa: TID251
BROWSERBASE_DEFAULT_SESSION_TIMEOUT_SECONDS = 600
BROWSERBASE_MAX_SESSION_TIMEOUT_SECONDS = 1800
BROWSERBASE_DOWNLOAD_TIMEOUT_SECONDS = 120
POSTMARK_TRACKING_HOST = "track.pstmrk.it"
MAX_DOWNLOAD_BYTES = 100 * 1024 * 1024
CDP_MAX_MESSAGE_BYTES = 256 * 1024 * 1024
LOGGER = logging.getLogger(__name__)
if not LOGGER.handlers:
    handler = logging.StreamHandler()
    handler.setFormatter(logging.Formatter("%(asctime)s docsend: %(message)s", datefmt="%H:%M:%S"))
    LOGGER.addHandler(handler)
LOGGER.setLevel(logging.INFO)
LOGGER.propagate = False


def _browserbase_api_key() -> str:
    # Server mode returns the stub key name; the proxy swaps it for the real
    # key in the API request header. Local mode uses .env when present
    # and otherwise keeps the stub so sandbox runs do not fail before proxying.
    return secret("BROWSERBASE_API_KEY", "BROWSERBASE_API_KEY")


def _prepare_cdp_tls() -> None:
    cert_path = "/firewall-certs/ca-cert.pem"
    if os.path.exists(cert_path):
        if not os.environ.get("NODE_EXTRA_CA_CERTS"):  # noqa: TID251
            os.environ["NODE_EXTRA_CA_CERTS"] = cert_path
        if not os.environ.get("SSL_CERT_FILE"):  # noqa: TID251
            os.environ["SSL_CERT_FILE"] = cert_path
    node_options = os.environ.get("NODE_OPTIONS", "")  # noqa: TID251
    if "--use-system-ca" not in node_options.split():
        os.environ["NODE_OPTIONS"] = f"{node_options} --use-system-ca".strip()


def _cdp_proxy_url(connect_url: str) -> str | None:
    """Resolve the standard environment proxy for a Browserbase CDP WebSocket."""
    return get_websocket_proxy(parse_websocket_uri(connect_url))


async def _pump_websocket_messages(source, destination) -> None:
    async for message in source:
        await destination.send(message)


async def _relay_cdp_connection(downstream, *, connect_url: str, proxy_url: str) -> None:
    try:
        async with websocket_connect(
            connect_url,
            proxy=proxy_url,
            max_size=CDP_MAX_MESSAGE_BYTES,
        ) as upstream:
            pumps = {
                asyncio.create_task(_pump_websocket_messages(downstream, upstream)),
                asyncio.create_task(_pump_websocket_messages(upstream, downstream)),
            }
            _, pending = await asyncio.wait(pumps, return_when=asyncio.FIRST_COMPLETED)
            for task in pending:
                task.cancel()
            await asyncio.gather(*pumps, return_exceptions=True)
    except Exception as exc:
        LOGGER.error("CDP proxy relay failed (%s)", type(exc).__name__)
        with suppress(Exception):
            await downstream.close(code=1011, reason="Upstream CDP connection failed")


@asynccontextmanager
async def _cdp_endpoint(connect_url: str) -> AsyncIterator[str]:
    """Route the CDP WebSocket through the environment proxy when configured."""
    proxy_url = _cdp_proxy_url(connect_url)
    if proxy_url is None:
        yield connect_url
        return

    handler = partial(
        _relay_cdp_connection,
        connect_url=connect_url,
        proxy_url=proxy_url,
    )
    async with websocket_serve(
        handler,
        "127.0.0.1",
        0,
        max_size=CDP_MAX_MESSAGE_BYTES,
    ) as relay:
        sockets = relay.sockets
        if not sockets:
            raise RuntimeError("Failed to start the local CDP proxy relay")
        port = sockets[0].getsockname()[1]
        yield f"ws://127.0.0.1:{port}"


def _browserbase_proxy_config() -> bool | list[dict]:
    country = BROWSERBASE_PROXY_COUNTRY.strip()
    if not country:
        return True
    return [
        {
            "type": "browserbase",
            "geolocation": {"country": country.upper()},
        }
    ]


def _validate_browserbase_session(
    session: object,
    *,
    require_connect_url: bool = True,
) -> dict:
    session_id = session.id
    connect_url = session.connect_url
    if not isinstance(session_id, str) or not session_id:
        raise RuntimeError("Browserbase session response did not include a session ID")
    if require_connect_url and (not isinstance(connect_url, str) or not connect_url):
        raise RuntimeError("Browserbase session response did not include a connect URL")
    expires_at = session.expires_at
    return {
        "id": session_id,
        "connectUrl": connect_url if isinstance(connect_url, str) else None,
        "expiresAt": str(expires_at) if expires_at is not None else None,
        "status": session.status,
        "userMetadata": session.user_metadata,
    }


def _validate_browserbase_session_id(session_id: str) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_-]+", session_id):
        raise ValueError("Invalid Browserbase session ID")


def _normalize_docsend_url(url: str) -> str:
    normalized = url.strip().rstrip("/")
    if not re.match(r"https?://", normalized):
        normalized = f"https://{normalized}"
    parsed = urlparse(normalized)
    hostname = (parsed.hostname or "").lower()
    if parsed.scheme not in {"http", "https"} or not (
        hostname == "docsend.com" or hostname.endswith(".docsend.com")
    ):
        raise ValueError("URL must be hosted on docsend.com")
    return normalized


def _encode_session_url(url: str) -> str:
    """Encode a URL using characters accepted by Browserbase metadata."""
    encoded = base64.urlsafe_b64encode(url.encode()).decode().rstrip("=")
    return f"b64{encoded}"


def _decode_session_url(value: str) -> str:
    """Decode a URL stored in Browserbase session metadata."""
    if not value.startswith("b64"):
        raise ValueError("Invalid DocSend URL in resumable session metadata")
    try:
        url = base64.urlsafe_b64decode(value[3:] + "===").decode()
    except (UnicodeDecodeError, ValueError) as exc:
        raise ValueError("Invalid DocSend URL in resumable session metadata") from exc
    return _normalize_docsend_url(url)


def _normalize_verification_url(url: str) -> str:
    normalized = url.strip().rstrip("/")
    parsed = urlparse(normalized)
    hostname = (parsed.hostname or "").lower()
    is_docsend = hostname == "docsend.com" or hostname.endswith(".docsend.com")
    if (
        parsed.scheme != "https"
        or parsed.username
        or parsed.password
        or not (is_docsend or hostname == POSTMARK_TRACKING_HOST)
    ):
        raise ValueError("Verification URL must be an HTTPS DocSend or Postmark URL")
    return normalized


def _space_identifier(url: str) -> str | None:
    match = re.match(r"^/view/s/([A-Za-z0-9]+)(?:/|$)", urlparse(url).path)
    return match.group(1) if match else None


def _normalize_space_url(url: str) -> str:
    normalized = _normalize_docsend_url(url)
    if not re.fullmatch(r"/view/s/[A-Za-z0-9]+/?", urlparse(normalized).path):
        raise ValueError("URL must be a DocSend Space URL under /view/s/")
    return normalized


def _matches_docsend_target(current_url: str, target_url: str) -> bool:
    current = urlparse(current_url)
    current_host = (current.hostname or "").lower()
    if not (current_host == "docsend.com" or current_host.endswith(".docsend.com")):
        return False
    target_space = _space_identifier(target_url)
    if target_space is not None:
        return _space_identifier(current_url) == target_space
    target = urlparse(target_url)
    return current.path.rstrip("/") == target.path.rstrip("/")


def _session_timeout(value: int) -> int:
    if not 60 <= value <= BROWSERBASE_MAX_SESSION_TIMEOUT_SECONDS:
        raise ValueError(
            "session_timeout must be between 60 and "
            f"{BROWSERBASE_MAX_SESSION_TIMEOUT_SECONDS} seconds"
        )
    return value


def _format_browserbase_error(error: Exception, api_key: str) -> str:
    if isinstance(error, APIStatusError):
        body = error.body
        detail = ""
        if isinstance(body, dict):
            detail = str(body.get("message") or body.get("error") or "")
        message = f"Browserbase API error {error.status_code}"
        if detail:
            message = f"{message}: {detail[:1000]}"
        return _redact_browserbase_secret(RuntimeError(message), api_key)
    return _redact_browserbase_secret(error, api_key)


async def _create_browserbase_session(api_key: str, url: str, session_timeout: int) -> dict:
    metadata = {"tool": "docsend", "docsend_url": _encode_session_url(url)}
    if len(json.dumps(metadata, separators=(",", ":"))) >= 512:
        raise ValueError("DocSend URL is too long for a resumable Browserbase session")
    async with AsyncBrowserbase(api_key=api_key) as browserbase:
        session = await browserbase.sessions.create(
            proxies=_browserbase_proxy_config(),
            keep_alive=True,
            api_timeout=_session_timeout(session_timeout),
            user_metadata=metadata,
        )
    return _validate_browserbase_session(session)


async def _get_browserbase_session(
    api_key: str,
    session_id: str,
    *,
    require_connect_url: bool = True,
) -> dict:
    _validate_browserbase_session_id(session_id)
    async with AsyncBrowserbase(api_key=api_key) as browserbase:
        session = await browserbase.sessions.retrieve(session_id)
    return _validate_browserbase_session(session, require_connect_url=require_connect_url)


async def _release_browserbase_session(api_key: str, session_id: str) -> None:
    _validate_browserbase_session_id(session_id)
    async with AsyncBrowserbase(api_key=api_key) as browserbase:
        await browserbase.sessions.update(
            session_id,
            status="REQUEST_RELEASE",
        )


async def _browserbase_download_records(api_key: str, session_id: str) -> list[dict]:
    """List granular Browserbase download records through the official SDK client."""
    _validate_browserbase_session_id(session_id)
    async with AsyncBrowserbase(api_key=api_key) as browserbase:
        # browserbase 1.15 does not yet expose a typed downloads resource, so use
        # its public low-level request method while retaining SDK auth and retries.
        response_text = await browserbase.get(
            "/v1/downloads",
            cast_to=str,
            options={"params": {"sessionId": session_id, "limit": 100}},
        )
    try:
        response = json.loads(response_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError("Browserbase returned an invalid downloads response") from exc
    downloads = response.get("downloads") if isinstance(response, dict) else None
    if not isinstance(downloads, list):
        raise RuntimeError("Browserbase returned an invalid downloads list")
    return [download for download in downloads if isinstance(download, dict)]


async def _browserbase_download_content(api_key: str, download: dict) -> tuple[str, bytes]:
    download_id = download.get("id")
    filename = download.get("filename")
    size = download.get("size")
    if not isinstance(download_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", download_id):
        raise RuntimeError("Browserbase returned an invalid download ID")
    if not isinstance(filename, str) or not filename.strip():
        raise RuntimeError("Browserbase returned a download without a filename")
    if isinstance(size, int) and size > MAX_DOWNLOAD_BYTES:
        raise RuntimeError(
            f"Downloaded file exceeds the {MAX_DOWNLOAD_BYTES // (1024 * 1024)} MB limit"
        )
    async with AsyncBrowserbase(api_key=api_key) as browserbase:
        content = await browserbase.get(
            f"/v1/downloads/{download_id}",
            cast_to=bytes,
            options={"headers": {"Accept": "application/octet-stream"}},
        )
    if len(content) > MAX_DOWNLOAD_BYTES:
        raise RuntimeError(
            f"Downloaded file exceeds the {MAX_DOWNLOAD_BYTES // (1024 * 1024)} MB limit"
        )
    return filename.strip(), content


class _SessionUnavailable(RuntimeError):
    def __init__(self, message: str, status: str):
        super().__init__(message)
        self.status = status


async def _load_docsend_session(
    api_key: str,
    session_id: str,
    *,
    require_running: bool = True,
) -> tuple[dict, str]:
    try:
        session = await _get_browserbase_session(
            api_key,
            session_id,
            require_connect_url=require_running,
        )
    except Exception as exc:
        raise _SessionUnavailable(
            _format_browserbase_error(exc, api_key),
            "session_unavailable",
        ) from exc

    metadata = session.get("userMetadata")
    if not isinstance(metadata, dict) or metadata.get("tool") != "docsend":
        raise _SessionUnavailable(
            "The Browserbase session does not belong to the DocSend tool.",
            "invalid_resume_session",
        )
    try:
        url = _decode_session_url(str(metadata["docsend_url"]))
    except ValueError as exc:
        raise _SessionUnavailable(str(exc), "invalid_resume_session") from exc
    except KeyError as exc:
        raise _SessionUnavailable(
            "The resumable session is missing its DocSend URL",
            "invalid_resume_session",
        ) from exc

    status = str(session.get("status", "")).upper()
    if require_running and status not in {"PENDING", "RUNNING"}:
        raise _SessionUnavailable(
            "The Browserbase session has expired or ended.",
            "session_expired",
        )
    return session, url


def _redact_browserbase_secret(
    error: Exception,
    api_key: str,
    connect_url: str | None = None,
    sensitive_url: str | None = None,
) -> str:
    text = str(error)
    if api_key:
        text = text.replace(api_key, "<redacted>")
    if connect_url:
        text = text.replace(connect_url, "wss://connect.browserbase.com?<redacted>")
    if sensitive_url:
        text = text.replace(sensitive_url, "https://docsend.com/<redacted>")
    return re.sub(r"apiKey=[^&\\s]+", "apiKey=<redacted>", text)


class DocsendClient:
    """Download DocSend documents as PDF via cloud browser."""

    def login(
        self,
        url: str,
        email: str = "",
        passcode: str | None = None,
        session_timeout: int = BROWSERBASE_DEFAULT_SESSION_TIMEOUT_SECONDS,
    ) -> dict:
        """Authenticate and open a DocSend Space in a resumable browser session.

        Args:
            url: DocSend Space URL under /view/s/.
            email: Email for email-gated Spaces.
            passcode: Passcode for protected Spaces.
            session_timeout: Browserbase session lifetime in seconds. Must be
                between 60 and 1800 seconds.

        Returns:
            Dict containing verification state or the Space inventory and session ID.
        """
        try:
            url = _normalize_space_url(url)
        except ValueError as exc:
            return _err(str(exc), status="invalid_space_url")
        return self._run_sync(
            self._run(url, email, passcode, None, session_timeout=session_timeout)
        )

    def download(
        self,
        url: str,
        email: str = "",
        passcode: str | None = None,
        session_timeout: int = BROWSERBASE_DEFAULT_SESSION_TIMEOUT_SECONDS,
    ) -> dict:
        """Download a DocSend document as PDF.

        Args:
            url: DocSend URL (e.g. https://docsend.com/view/abc123)
            email: Email for email-gated documents.
            passcode: Passcode for password-protected documents.
            session_timeout: Browserbase session lifetime in seconds. Must be
                between 60 and 1800 seconds.

        Returns:
            Dict with status, filename, data (base64), page_count, etc.
        """
        return self._run_sync(
            self._run(url, email, passcode, None, session_timeout=session_timeout)
        )

    def resume(self, session_id: str, verification_link: str) -> dict:
        """Resume an email-gated DocSend download in its original browser session.

        Args:
            session_id: Browserbase session ID returned by download() when its
                status is verification_link_required.
            verification_link: Full verification URL sent by DocSend.

        Returns:
            Dict with status, filename, data (base64), page_count, etc.
        """
        if not verification_link:
            return _err("verification_link is required", status="verification_link_required")
        try:
            verification_link = _normalize_verification_url(verification_link)
        except ValueError as exc:
            return _err(str(exc), status="verification_link_invalid")
        return self._run_sync(self._run("", "", None, verification_link, session_id))

    def list_space(self, session_id: str) -> dict:
        """List the root of a verified DocSend Space.

        Args:
            session_id: Active Browserbase session ID returned by download() or resume().

        Returns:
            Dict containing root items, stable IDs, and session expiry.
        """
        return self._run_sync(self._run_space_command(session_id, action="list"))

    def open_folder(self, session_id: str, folder_id: str) -> dict:
        """Open one folder in a verified DocSend Space.

        Args:
            session_id: Active Browserbase session ID returned by login() or resume().
            folder_id: Folder item ID returned by list_space() or open_folder().

        Returns:
            Dict containing the folder's immediate children and session expiry.
        """
        folder = _decode_space_item_id(folder_id)
        if folder is None or folder["type"] != "folder":
            return _err(
                "folder_id must identify a folder returned by docsend list or open-folder",
                status="invalid_folder",
            )
        return self._run_sync(
            self._run_space_command(
                session_id,
                action="open_folder",
                folder=folder,
            )
        )

    def fetch(self, session_id: str, item_id: str) -> dict:
        """Download one item from a verified DocSend Space.

        Args:
            session_id: Active Browserbase session ID returned by download() or resume().
            item_id: Stable item ID returned by list_space() or open_folder().

        Returns:
            Dict containing the original file as base64 when downloading is enabled,
            or a recovered PDF when DocSend renders the document without a download button.
        """
        item = _decode_space_item_id(item_id)
        if item is None:
            return _err("item_id must identify an item returned by DocSend", status="invalid_item")
        return self._run_sync(self._run_space_command(session_id, action="fetch", item=item))

    def close_session(self, session_id: str) -> dict:
        """Release a resumable DocSend Browserbase session.

        Args:
            session_id: Browserbase session ID to release.

        Returns:
            Dict confirming whether the session was released or had already ended.
        """
        return self._run_sync(self._close_session(session_id))

    def _run_sync(self, coroutine) -> dict:
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = None
        if loop and loop.is_running():
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                return pool.submit(asyncio.run, coroutine).result()
        return asyncio.run(coroutine)

    async def _close_session(self, session_id: str) -> dict:
        api_key = _browserbase_api_key()
        try:
            session = await _get_browserbase_session(
                api_key,
                session_id,
                require_connect_url=False,
            )
            metadata = session.get("userMetadata")
            if not isinstance(metadata, dict) or metadata.get("tool") != "docsend":
                return _err(
                    "The Browserbase session does not belong to the DocSend tool.",
                    status="invalid_resume_session",
                )
            status = str(session.get("status", "")).upper()
            if status not in {"PENDING", "RUNNING"}:
                return {
                    "status": "closed",
                    "session_id": session_id,
                    "already_closed": True,
                    "error": None,
                }
            LOGGER.info("Releasing Browserbase session %s", session_id)
            await _release_browserbase_session(api_key, session_id)
            LOGGER.info("Released Browserbase session %s", session_id)
            return {
                "status": "closed",
                "session_id": session_id,
                "already_closed": False,
                "error": None,
            }
        except Exception as exc:
            return _err(_format_browserbase_error(exc, api_key))

    async def _run_space_command(
        self,
        session_id: str,
        *,
        action: str,
        item: dict | None = None,
        folder: dict | None = None,
    ) -> dict:
        api_key = _browserbase_api_key()
        try:
            from playwright.async_api import async_playwright
        except ImportError:
            return _err("playwright not installed")

        try:
            session, url = await _load_docsend_session(api_key, session_id)
        except _SessionUnavailable as exc:
            return _err(str(exc), status=exc.status)

        connect_url = str(session["connectUrl"])
        expires_at = session.get("expiresAt")
        try:
            _prepare_cdp_tls()
            async with _cdp_endpoint(connect_url) as cdp_url, async_playwright() as playwright:
                LOGGER.info("Attaching to DocSend Space session %s", session_id)
                browser = await playwright.chromium.connect_over_cdp(cdp_url)
                context = browser.contexts[0] if browser.contexts else await browser.new_context()
                page = _find_existing_space_page(context.pages, url)
                if page is None:
                    return _err(
                        "The active DocSend Space tab is no longer available.",
                        status="session_unavailable",
                    )
                LOGGER.info("Reusing the existing DocSend Space page")

                await _dismiss_cookies(page)
                if await _has_verification_wall(page):
                    result = _err(
                        "The DocSend Space still requires email verification.",
                        status="verification_link_required",
                    )
                    result["resume_session_id"] = session_id
                    result["expires_at"] = expires_at
                    return result
                if action == "fetch" and item is not None:
                    result = await _fetch_space_item(
                        browser,
                        page,
                        api_key,
                        session_id,
                        item,
                    )
                    result["session_id"] = session_id
                    result["expires_at"] = expires_at
                    return result

                if action == "list":
                    await _navigate_space_root(page)
                    parent_path = ""
                elif action == "open_folder" and folder is not None:
                    await _navigate_space_folder_path(page, folder["path"])
                    parent_path = folder["path"]
                else:
                    return _err("Unsupported DocSend Space action", status="invalid_action")

                title, items = await _extract_space_inventory(page, parent_path)

                return _deal_room_result(
                    session_id=session_id,
                    expires_at=expires_at,
                    title=title,
                    items=items,
                    folder=folder,
                )
        except Exception as exc:
            error = _redact_browserbase_secret(exc, api_key, connect_url)
            LOGGER.error("DocSend Space command failed: %s", error)
            return _err(error)

    async def _run(
        self,
        url: str,
        email: str,
        passcode: str | None,
        verification_link: str | None,
        resume_session_id: str | None = None,
        session_timeout: int = BROWSERBASE_DEFAULT_SESSION_TIMEOUT_SECONDS,
    ) -> dict:
        api_key = _browserbase_api_key()

        try:
            from playwright.async_api import async_playwright
        except ImportError:
            return _err("playwright not installed")

        try:
            if resume_session_id:
                LOGGER.info("Loading resumable Browserbase session %s", resume_session_id)
                try:
                    session, url = await _load_docsend_session(api_key, resume_session_id)
                except _SessionUnavailable as exc:
                    if exc.status == "session_expired":
                        return _err(
                            "The resumable Browserbase session has expired or ended. "
                            "Start the DocSend download again.",
                            status="verification_session_expired",
                        )
                    return _err(str(exc), status=exc.status)
            else:
                url = _normalize_docsend_url(url)
                session_timeout = _session_timeout(session_timeout)
                LOGGER.info(
                    "Creating Browserbase session with a %d-second maximum lifetime",
                    session_timeout,
                )
                session = await _create_browserbase_session(api_key, url, session_timeout)
        except Exception as e:
            error = _format_browserbase_error(e, api_key)
            LOGGER.error("Browserbase session setup failed: %s", error)
            return _err(error)

        session_id = str(session["id"])
        connect_url = str(session["connectUrl"])
        expires_at = session.get("expiresAt")
        keep_session = False
        LOGGER.info("Browserbase session %s is ready; expires at %s", session_id, expires_at)

        try:
            _prepare_cdp_tls()
            async with _cdp_endpoint(connect_url) as cdp_url, async_playwright() as p:
                LOGGER.info("Connecting Playwright to Browserbase session %s", session_id)
                browser = await p.chromium.connect_over_cdp(cdp_url)
                ctx = browser.contexts[0] if browser.contexts else await browser.new_context()
                page = ctx.pages[0] if ctx.pages else await ctx.new_page()

                # If we have a verification link, open it first
                if verification_link:
                    LOGGER.info("Opening the DocSend verification link")
                    try:
                        await page.goto(
                            verification_link, wait_until="domcontentloaded", timeout=30000
                        )
                    except Exception:
                        LOGGER.warning(
                            "Verification-link navigation timed out; continuing with the session"
                        )
                    await asyncio.sleep(5)

                # 3. Navigate only when this session is not already on the target.
                if not _matches_docsend_target(page.url, url):
                    LOGGER.info("Opening the DocSend document")
                    try:
                        await page.goto(url, wait_until="networkidle", timeout=45000)
                    except Exception:
                        LOGGER.warning(
                            "Document navigation did not reach network idle; inspecting the page"
                        )
                else:
                    LOGGER.info("Reusing the existing DocSend page")

                # 4. Detect state
                state = await _detect_state(page)
                LOGGER.info("Detected DocSend state: %s", state)
                if state == "expired":
                    return _err("Document not found or expired", status="expired")
                if state == "blocked":
                    return _err("Blocked by CloudFront/WAF", status="blocked")

                await _dismiss_cookies(page)

                if state == "passcode_required":
                    if not passcode:
                        return _err(
                            "This document is password-protected. Ask the user "
                            "for the passcode and retry with the passcode parameter.",
                            status="passcode_required",
                        )
                    LOGGER.info("Submitting the DocSend access form with a passcode")
                    ok = await _enter_passcode(page, email, passcode)
                    if not ok:
                        return _err("Passcode was rejected", status="passcode_required")

                elif state == "email_required" and not verification_link:
                    if not email:
                        return _err(
                            "This document requires an email address to access. "
                            "Ask the user for their email and retry with the "
                            "email parameter.",
                            status="email_required",
                        )
                    LOGGER.info("Submitting the DocSend email gate")
                    await _enter_email(page, email)
                    await asyncio.sleep(1)

                if await _has_verification_wall(page):
                    keep_session = True
                    if resume_session_id or verification_link:
                        message = (
                            "The verification link did not unlock the document. "
                            "Retry resume with a valid verification link before the "
                            "session expires."
                        )
                        verification_status = "verification_link_invalid"
                    else:
                        message = (
                            f"DocSend sent a verification link to {email}. "
                            "Find the link, then call resume with resume_session_id "
                            "and verification_link before the session expires."
                        )
                        verification_status = "verification_link_required"
                    result = _err(
                        message,
                        status=verification_status,
                    )
                    result["resume_session_id"] = session_id
                    result["expires_at"] = expires_at
                    LOGGER.info(
                        "Pausing for verification; session %s remains available until %s",
                        session_id,
                        expires_at,
                    )
                    return result

                if verification_link and state == "email_required":
                    keep_session = True
                    result = _err(
                        "The verification link did not unlock the document. "
                        "Retry resume with a valid verification link before the session expires.",
                        status="verification_link_invalid",
                    )
                    result["resume_session_id"] = session_id
                    result["expires_at"] = expires_at
                    LOGGER.info(
                        "Verification did not unlock the document; session %s remains available",
                        session_id,
                    )
                    return result

                if _space_identifier(url) is not None:
                    await _navigate_space_root(page)
                    title, items = await _extract_space_inventory(page, "")
                    LOGGER.info("Detected a DocSend Space with %d items", len(items))
                    keep_session = True
                    return _deal_room_result(
                        session_id=session_id,
                        expires_at=expires_at,
                        title=title,
                        items=items,
                    )

                slug_m = re.search(r"docsend\.com/view/(?:s/)?([a-zA-Z0-9]+)", url)
                slug = slug_m.group(1) if slug_m else "document"
                result = await _recover_rendered_document(
                    page,
                    filename=f"docsend_{slug}.pdf",
                )
                if result["status"] == "ok":
                    LOGGER.info("DocSend download completed successfully")
                return result

        except Exception as e:
            error = _redact_browserbase_secret(
                e,
                api_key,
                connect_url,
                sensitive_url=verification_link,
            )
            LOGGER.error("DocSend download failed: %s", error)
            return _err(error)
        finally:
            if not keep_session:
                try:
                    LOGGER.info("Releasing Browserbase session %s", session_id)
                    await _release_browserbase_session(api_key, session_id)
                    LOGGER.info("Released Browserbase session %s", session_id)
                except Exception as e:
                    LOGGER.warning(
                        "Failed to release Browserbase session %s: %s",
                        session_id,
                        _format_browserbase_error(e, api_key),
                    )


# ---------------------------------------------------------------------------
# State detection
# ---------------------------------------------------------------------------


async def _detect_state(page) -> str:
    title = (await page.title()).lower()
    if "404" in title or "not found" in title:
        return "expired"
    if "request could not be satisfied" in title:
        return "blocked"

    for sel in ['input[type="password"]', "#link_auth_form_passcode", 'input[name*="passcode"]']:
        el = await page.query_selector(sel)
        if el:
            try:
                box = await el.bounding_box()
                if box and box["width"] > 30:
                    return "passcode_required"
            except Exception:
                pass

    for sel in [
        '#prompt input[type="email"]',
        '.ReactModal__Content input[type="email"]',
        '[class*="auth"] input[type="email"]',
        '.modal input[type="email"]',
    ]:
        try:
            loc = page.locator(sel).first
            if await loc.count() > 0:
                box = await loc.bounding_box(timeout=2000)
                if box and box["width"] > 50:
                    return "email_required"
        except Exception:
            continue

    try:
        body = await page.inner_text("body")
        if body and "no longer available" in body.lower():
            return "expired"
    except Exception:
        pass

    return "ready"


async def _has_verification_wall(page) -> bool:
    """Check if DocSend is showing a 'verify your email' wall after submission."""
    try:
        body = await page.inner_text("body")
        lower = body.lower()
        return any(
            phrase in lower
            for phrase in [
                "requests your action",
                "emailed a link",
                "verify that you own",
                "verification link",
                "check your email",
            ]
        )
    except Exception:
        return False


# ---------------------------------------------------------------------------
# Auth helpers
# ---------------------------------------------------------------------------


async def _dismiss_cookies(page) -> None:
    iframe = page.locator("iframe#ccpa-iframe")
    if await iframe.count() == 0:
        return
    button = page.frame_locator("iframe#ccpa-iframe").locator("#onetrust-accept-btn-handler")
    try:
        if not await button.is_visible(timeout=500):
            return
        await button.click(force=True, timeout=5000)
        LOGGER.info("Accepted the DocSend cookie banner in ccpa-iframe")
        await asyncio.sleep(1)
    except Exception:
        LOGGER.warning("The DocSend cookie banner could not be dismissed")


async def _enter_email(page, email: str) -> None:
    for sel in [
        "#link_auth_form_email",
        '#new_link_auth_form input[type="email"]',
        '#prompt input[type="email"]',
        '.ReactModal__Content input[type="email"]',
        '#email[type="email"]',
        'input[type="email"]',
    ]:
        try:
            loc = page.locator(sel).first
            if await loc.is_visible(timeout=2000):
                await loc.fill(email)
                await asyncio.sleep(0.5)
                break
        except Exception:
            continue
    await _click_submit(page)
    with suppress(Exception):
        await page.wait_for_load_state("networkidle", timeout=15000)
    await asyncio.sleep(2)


async def _enter_passcode(page, email: str, passcode: str) -> bool:
    for sel in [
        "#link_auth_form_email",
        '#new_link_auth_form input[type="email"]',
        '.ReactModal__Content input[type="email"]',
        '#email[type="email"]',
    ]:
        try:
            loc = page.locator(sel).first
            if await loc.is_visible(timeout=2000):
                await loc.fill(email)
                await asyncio.sleep(0.5)
                break
        except Exception:
            continue

    for sel in ["#link_auth_form_passcode", 'input[type="password"]', 'input[name*="passcode"]']:
        try:
            loc = page.locator(sel).first
            if await loc.is_visible(timeout=2000):
                await loc.fill(passcode)
                await asyncio.sleep(0.5)
                break
        except Exception:
            continue

    await _click_submit(page)
    with suppress(Exception):
        await page.wait_for_load_state("networkidle", timeout=15000)
    await asyncio.sleep(2)

    for sel in ['input[type="password"]', "#link_auth_form_passcode", 'input[name*="passcode"]']:
        try:
            if await page.locator(sel).first.is_visible(timeout=1000):
                return False
        except Exception:
            pass
    return True


async def _click_submit(page) -> None:
    for sel in [
        'button:has-text("Continue")',
        'button:has-text("Submit")',
        'button:has-text("Confirm")',
        'button:has-text("Enter")',
        'button[type="submit"]',
    ]:
        try:
            btn = page.locator(sel).first
            if await btn.is_visible(timeout=1500):
                await btn.click()
                return
        except Exception:
            continue
    await page.keyboard.press("Enter")


# ---------------------------------------------------------------------------
# DocSend Spaces
# ---------------------------------------------------------------------------


SPACE_SELECTOR_TIMEOUT_MS = 15_000


def _find_existing_space_page(pages, url: str):
    expected_identifier = _space_identifier(url)
    if expected_identifier is None:
        return None
    for page in reversed(pages):
        parsed = urlparse(page.url)
        hostname = (parsed.hostname or "").lower()
        if (hostname == "docsend.com" or hostname.endswith(".docsend.com")) and _space_identifier(
            page.url
        ) == expected_identifier:
            return page
    return None


async def _space_table(page):
    table = page.get_by_test_id("space-table")
    await table.wait_for(state="visible", timeout=SPACE_SELECTOR_TIMEOUT_MS)
    return table


async def _space_folder_heading(page) -> str:
    table = await _space_table(page)
    headings = table.locator("h2")
    if await headings.count() != 1:
        raise RuntimeError("Could not determine the current DocSend Space folder")
    return (await headings.first.inner_text()).strip()


async def _navigate_space_root(page) -> None:
    if await _space_folder_heading(page) == "Home":
        return
    home_buttons = page.get_by_role("button", name="Home", exact=True)
    if await home_buttons.count() != 1:
        raise RuntimeError("Could not locate the DocSend Space Home control")
    LOGGER.info("Opening the DocSend Space root")
    await home_buttons.first.click(timeout=SPACE_SELECTOR_TIMEOUT_MS)
    await (
        (await _space_table(page))
        .get_by_role(
            "heading",
            name="Home",
            exact=True,
        )
        .wait_for(state="visible", timeout=SPACE_SELECTOR_TIMEOUT_MS)
    )


async def _navigate_space_folder_path(page, folder_path: str) -> None:
    await _navigate_space_root(page)
    folder_names = [part.strip() for part in folder_path.split("/") if part.strip()]
    for folder_name in folder_names:
        LOGGER.info("Opening DocSend Space folder %s", folder_name)
        row = await _find_unique_space_row(page, folder_name, folder=True)
        if row is None:
            raise RuntimeError(
                f"Could not uniquely locate folder {folder_name!r} in the DocSend Space"
            )
        await row.get_by_role("link", name=folder_name, exact=True).click(
            timeout=SPACE_SELECTOR_TIMEOUT_MS
        )
        await (
            (await _space_table(page))
            .get_by_role(
                "heading",
                name=folder_name,
                exact=True,
            )
            .wait_for(state="visible", timeout=SPACE_SELECTOR_TIMEOUT_MS)
        )


async def _extract_space_inventory(page, parent_path: str) -> tuple[str, list[dict]]:
    table = await _space_table(page)
    rows = table.get_by_role("row")
    items: list[dict] = []
    for index in range(await rows.count()):
        row = rows.nth(index)
        links = row.locator("a[href]")
        if await links.count() < 1:
            continue
        link = links.first
        name = " ".join((await link.inner_text()).split())
        href = await link.get_attribute("href")
        if not name or not href:
            continue

        href_path = urlparse(href).path
        folder_marker = row.get_by_test_id("folder-icon-small")
        download_button = row.locator('button[aria-label="Download file"]')
        is_folder = await folder_marker.count() > 0 or bool(
            re.match(r"^/view/s/[A-Za-z0-9]+/f/", href_path)
        )
        is_file = await download_button.count() > 0 or bool(
            re.match(r"^/view/[A-Za-z0-9]+/d/", href_path)
        )
        if is_folder:
            item_type = "folder"
            downloadable = None
            download_method = None
        elif is_file:
            item_type = "file"
            downloadable = await download_button.count() == 1 and await download_button.is_enabled()
            download_method = "original" if downloadable else "rendered_pdf"
        else:
            item_type = "url"
            downloadable = None
            download_method = None

        path = f"{parent_path.rstrip('/')}/{name}" if parent_path else name
        items.append(
            _make_space_item(
                name=name,
                path=path,
                item_type=item_type,
                downloadable=downloadable,
                download_method=download_method,
            )
        )

    title = (await page.title()).strip()
    title = title.removesuffix(" — DocSend").removesuffix(" - DocSend").strip()
    return title, items


async def _configure_browserbase_downloads(browser) -> None:
    cdp_session = await browser.new_browser_cdp_session()
    await cdp_session.send(
        "Browser.setDownloadBehavior",
        {
            "behavior": "allow",
            "downloadPath": "downloads",
            "eventsEnabled": True,
        },
    )


async def _wait_for_new_download(
    api_key: str,
    session_id: str,
    existing_ids: set[str],
) -> tuple[str, bytes] | None:
    deadline = asyncio.get_running_loop().time() + BROWSERBASE_DOWNLOAD_TIMEOUT_SECONDS
    while asyncio.get_running_loop().time() < deadline:
        downloads = await _browserbase_download_records(api_key, session_id)
        new_downloads = [
            download
            for download in downloads
            if isinstance(download.get("id"), str) and download["id"] not in existing_ids
        ]
        if new_downloads:
            return await _browserbase_download_content(api_key, new_downloads[0])
        await asyncio.sleep(1)
    return None


def _make_space_item(
    *,
    name: str,
    path: str,
    item_type: str,
    downloadable: bool | None,
    download_method: str | None = None,
) -> dict:
    identity = json.dumps(
        {"path": path, "type": item_type},
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    encoded_identity = base64.urlsafe_b64encode(identity.encode()).decode().rstrip("=")
    item_id = f"item_{encoded_identity}"
    return {
        "id": item_id,
        "name": name,
        "path": path,
        "type": item_type,
        "downloadable": downloadable,
        "download_method": download_method,
    }


def _decode_space_item_id(item_id: str | None) -> dict | None:
    if not isinstance(item_id, str) or not item_id.startswith("item_"):
        return None
    encoded = item_id.removeprefix("item_")
    if not encoded or len(encoded) > 4096:
        return None
    try:
        padding = "=" * (-len(encoded) % 4)
        identity = base64.b64decode(
            encoded + padding,
            altchars=b"-_",
            validate=True,
        ).decode()
        payload = json.loads(identity)
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(payload, dict):
        return None
    path = payload.get("path")
    item_type = payload.get("type")
    if (
        not isinstance(path, str)
        or not path.strip()
        or len(path) > 1024
        or any(ord(character) < 32 for character in path)
        or item_type not in {"file", "folder", "url"}
    ):
        return None
    normalized_path = path.strip()
    return {
        "id": item_id,
        "name": normalized_path.rsplit("/", 1)[-1],
        "path": normalized_path,
        "type": item_type,
    }


def _deal_room_result(
    *,
    session_id: str,
    expires_at: object,
    title: str,
    items: list[dict],
    folder: dict | None = None,
) -> dict:
    return {
        "status": "deal_room_ready",
        "session_id": session_id,
        "expires_at": expires_at,
        "title": title,
        "folder_id": folder["id"] if folder else None,
        "folder_path": folder["path"] if folder else None,
        "items": items,
        "item_count": len(items),
        "error": None,
    }


async def _find_unique_space_row(page, name: str, *, folder: bool = False):
    table = page.get_by_test_id("space-table")
    await table.wait_for(state="visible", timeout=SPACE_SELECTOR_TIMEOUT_MS)
    rows = table.get_by_role("row").filter(has=page.get_by_role("link", name=name, exact=True))
    if folder:
        rows = rows.filter(has=page.get_by_test_id("folder-icon-small"))
    if await rows.count() != 1:
        return None
    return rows.first


async def _find_space_file_row(page, item: dict):
    parent_path, _, _ = item["path"].rpartition("/")
    try:
        if parent_path:
            await _navigate_space_folder_path(page, parent_path)
        else:
            await _navigate_space_root(page)
    except RuntimeError as exc:
        return None, _err(str(exc), status="item_not_found")

    row = await _find_unique_space_row(page, item["name"])
    if row is None:
        return None, _err(
            f"Could not uniquely locate file {item['name']!r} in the DocSend Space.",
            status="item_not_found",
        )
    return row, None


async def _click_space_file_download(page, item: dict, row=None) -> dict:
    if row is None:
        row, error = await _find_space_file_row(page, item)
        if error is not None:
            return error
    download_button = row.locator('button[aria-label="Download file"]')
    if await download_button.count() != 1 or not await download_button.is_enabled():
        return _err(
            "The document owner has not enabled downloading for this item.",
            status="not_downloadable",
        )

    LOGGER.info("Clicking the download control once for deal-room item %s", item["id"])
    await download_button.click(timeout=SPACE_SELECTOR_TIMEOUT_MS)
    return {"status": "ok", "error": None}


def _rendered_pdf_filename(name: str) -> str:
    filename = name.rsplit("/", 1)[-1].replace("\x00", "").strip() or "docsend_document"
    stem = re.sub(r"\.[A-Za-z0-9]{1,10}$", "", filename).strip() or "docsend_document"
    return f"{stem}.pdf"


async def _recover_space_document(page, item: dict, row) -> dict:
    links = row.locator("a[href]")
    if await links.count() < 1:
        return _err(
            "DocSend did not provide a viewer link for this item.",
            status="download_unavailable",
        )
    href = await links.first.get_attribute("href")
    if not href:
        return _err(
            "DocSend did not provide a viewer link for this item.",
            status="download_unavailable",
        )
    try:
        document_url = _normalize_docsend_url(urljoin(page.url, href))
    except ValueError as exc:
        return _err(str(exc), status="download_unavailable")

    LOGGER.info("Recovering rendered pages for deal-room item %s", item["id"])
    document_page = await page.context.new_page()
    try:
        try:
            await document_page.goto(document_url, wait_until="networkidle", timeout=45000)
        except Exception:
            LOGGER.warning(
                "Deal-room document navigation did not reach network idle; inspecting the page"
            )
        state = await _detect_state(document_page)
        if state == "expired":
            return _err("Document not found or expired", status="expired")
        if state == "blocked":
            return _err("Blocked by CloudFront/WAF", status="blocked")
        if state != "ready" or await _has_verification_wall(document_page):
            return _err(
                "The deal-room session did not unlock this document viewer.",
                status="download_unavailable",
            )
        await _dismiss_cookies(document_page)
        result = await _recover_rendered_document(
            document_page,
            filename=_rendered_pdf_filename(item["name"]),
        )
        if result["status"] == "ok":
            result["download_method"] = "rendered_pdf"
        return result
    finally:
        await document_page.close()


async def _fetch_space_item(
    browser,
    page,
    api_key: str,
    session_id: str,
    item: dict,
) -> dict:
    if item["type"] in {"folder", "url"}:
        return _err(
            f"{item['name']} is a {item['type']} and cannot be downloaded as a file.",
            status="not_downloadable",
        )
    row, error = await _find_space_file_row(page, item)
    if error is not None:
        return error
    download_button = row.locator('button[aria-label="Download file"]')
    has_original_download = (
        await download_button.count() == 1 and await download_button.is_enabled()
    )
    if not has_original_download:
        result = await _recover_space_document(page, item, row)
        result["item_id"] = item["id"]
        return result

    existing_downloads = await _browserbase_download_records(api_key, session_id)
    existing_ids = {
        download["id"] for download in existing_downloads if isinstance(download.get("id"), str)
    }
    await _configure_browserbase_downloads(browser)
    click_result = await _click_space_file_download(page, item, row)
    if click_result["status"] != "ok":
        return click_result
    downloaded = await _wait_for_new_download(
        api_key,
        session_id,
        existing_ids,
    )
    if not downloaded:
        return _err(
            "DocSend did not produce a downloadable file.",
            status="download_unavailable",
        )

    attachment_filename, content = downloaded
    filename = attachment_filename.rsplit("/", 1)[-1].replace("\x00", "").strip()
    if not filename:
        return _err("Browserbase returned an invalid download filename")
    mime_type = mimetypes.guess_type(filename)[0] or "application/octet-stream"
    LOGGER.info("Downloaded deal-room item %s as %s", item["id"], filename)
    return {
        "status": "ok",
        "item_id": item["id"],
        "filename": filename,
        "mime_type": mime_type,
        "size": len(content),
        "download_method": "original",
        "data": base64.b64encode(content).decode(),
        "error": None,
    }


# ---------------------------------------------------------------------------
# Slide extraction
# ---------------------------------------------------------------------------


async def _recover_rendered_document(page, *, filename: str) -> dict:
    """Recover a visible DocSend document from its rendered page images."""
    images = await _capture_spreadsheet_sheets(page)
    total = len(images)
    if total:
        LOGGER.info("Captured %d DocSend spreadsheet sheets", total)
    else:
        for _ in range(3):
            total = await _slide_count(page)
            if total > 0:
                break
            await asyncio.sleep(2)
        if total == 0:
            return _err("Could not determine page count")
        LOGGER.info("DocSend document contains %d pages", total)

        await _navigate_all_slides(page, total)

        LOGGER.info("Extracting rendered slide image URLs")
        image_urls = await _extract_dom_image_urls(page)
        if len(image_urls) == total:
            LOGGER.info("Found %d rendered slide images", len(image_urls))
            images = await _download_images(image_urls)
        else:
            LOGGER.info(
                "Found %d/%d rendered slide images; using the DocSend page-data fallback",
                len(image_urls),
                total,
            )
            api_urls = await _fetch_slide_urls(page, total)
            valid_urls = [url for url in api_urls if url]
            if len(valid_urls) != total:
                return _err(
                    f"Failed to recover all document pages ({len(valid_urls)}/{total}).",
                    page_count=total,
                )
            images = await _download_images(valid_urls)

        if len(images) != total:
            LOGGER.info(
                "Downloaded %d/%d rendered slide images; using the viewport capture fallback",
                len(images),
                total,
            )
            images = await _capture_visible_pages(page, total)
            if len(images) != total:
                return _err(
                    f"Failed to recover all document pages ({len(images)}/{total}).",
                    page_count=total,
                )

    LOGGER.info("Assembling %d downloaded pages into a PDF", len(images))
    buffer = BytesIO()
    images[0].save(
        buffer,
        "PDF",
        save_all=True,
        append_images=images[1:] if len(images) > 1 else [],
    )
    return {
        "status": "ok",
        "filename": filename,
        "data": base64.b64encode(buffer.getvalue()).decode(),
        "mime_type": "application/pdf",
        "page_count": total,
        "downloaded": len(images),
        "error": None,
    }


async def _slide_count(page) -> int:
    for sel in [".toolbar-page-indicator", ".page-label", '[class*="page-indicator"]']:
        try:
            el = await page.query_selector(sel)
            if el:
                text = await el.text_content()
                m = re.search(r"(\d+)\s*/\s*(\d+)", text or "")
                if m:
                    return int(m.group(2))
        except Exception:
            continue
    thumbs = await page.query_selector_all('[class*="document-thumb-container"]')
    if thumbs:
        nums = []
        for t in thumbs:
            n = await t.get_attribute("data-page-num")
            if n:
                nums.append(int(n))
        if nums:
            return max(nums)
    return 0


async def _navigate_all_slides(page, total: int) -> None:
    """Navigate through every slide to force DocSend to render them in the DOM."""
    LOGGER.info("Rendering all %d DocSend pages", total)
    # Go to page 1
    for _ in range(total):
        await page.keyboard.press("ArrowLeft")
        await asyncio.sleep(0.1)
    await asyncio.sleep(1)

    # Walk forward through every slide
    for page_number in range(2, total + 1):
        await page.keyboard.press("ArrowRight")
        await asyncio.sleep(0.3)
        if page_number == total or page_number % 10 == 0:
            LOGGER.info("Rendered page %d/%d", page_number, total)
    await asyncio.sleep(1)


async def _capture_visible_pages(page, total: int) -> list[Image.Image]:
    """Capture every page from the authenticated viewer when image URLs reject downloads.

    Spreadsheet-style DocSend viewers can render pages successfully while their signed
    image URLs return 403 outside the renderer. In that case, preserve exactly what the
    authenticated browser can display by stepping through the viewer and capturing each
    viewport as a PDF page.
    """
    LOGGER.info("Capturing all %d DocSend pages from the authenticated viewport", total)
    for _ in range(total):
        await page.keyboard.press("ArrowLeft")
        await asyncio.sleep(0.1)
    await asyncio.sleep(1)

    images: list[Image.Image] = []
    for page_number in range(1, total + 1):
        try:
            png = await _capture_visible_page(page)
            images.append(_rgb_image_from_png(png))
        except Exception as exc:
            LOGGER.warning("Failed to capture DocSend page %d: %s", page_number, exc)
            break
        if page_number < total:
            await page.keyboard.press("ArrowRight")
            await asyncio.sleep(0.75)
    LOGGER.info("Captured %d/%d DocSend pages from the viewport", len(images), total)
    return images


async def _capture_spreadsheet_sheets(page) -> list[Image.Image]:
    """Click and capture each sheet inside DocSend's spreadsheet preview iframe."""
    await _hide_capture_overlays(page)
    iframe = await page.query_selector("#previews-iframe")
    if iframe is None:
        return []
    frame = await iframe.content_frame()
    if frame is None:
        return []

    tabs = frame.locator("#tabstrip a.tabstrip-link")
    sheets = frame.locator(".sheet-content")
    tab_count = await tabs.count()
    if tab_count == 0 or await sheets.count() != tab_count:
        return []

    images: list[Image.Image] = []
    for index in range(tab_count):
        tab = tabs.nth(index)
        sheet = sheets.nth(index)
        name = (await tab.inner_text()).strip() or f"Sheet {index + 1}"
        LOGGER.info("Capturing DocSend spreadsheet tab %d/%d: %s", index + 1, tab_count, name)
        await tab.click(force=True)
        await frame.wait_for_timeout(500)
        await sheet.wait_for(state="visible")
        images.append(_rgb_image_from_png(await iframe.screenshot()))
    return images


def _rgb_image_from_png(png: bytes) -> Image.Image:
    source = Image.open(BytesIO(png))
    rgb = Image.new("RGB", source.size, (255, 255, 255))
    rgb.paste(source, mask=source.split()[3] if source.mode == "RGBA" else None)
    source.close()
    return rgb


async def _capture_visible_page(page) -> bytes:
    """Capture the document surface without DocSend's surrounding viewer chrome."""
    await _hide_capture_overlays(page)
    spreadsheet = page.locator(
        'iframe#previews-iframe:visible, iframe[class*="spreadsheet-viewer"]:visible'
    ).first
    if await spreadsheet.count() > 0:
        LOGGER.info("Capturing the visible DocSend spreadsheet frame")
        return await spreadsheet.screenshot()
    LOGGER.info("No document-only capture surface found; capturing the viewer viewport")
    return await page.screenshot(full_page=False)


async def _hide_capture_overlays(page) -> None:
    """Hide DocSend overlays that obscure documents or intercept workbook clicks."""
    await page.evaluate(
        """() => {
          const cookieFrame = document.querySelector('#ccpa-iframe');
          if (cookieFrame) cookieFrame.style.visibility = 'hidden';
        }"""
    )


async def _extract_dom_image_urls(page) -> list[str]:
    """Extract slide image URLs from rendered <img> elements in the DOM.

    DocSend's viewer renders slides as <img class="preso-view page-view">.
    After navigating through all slides, their .src attributes contain
    authenticated CloudFront URLs.
    """
    urls = await page.evaluate("""() => {
        const imgs = document.querySelectorAll("img.preso-view.page-view, img.page-view");
        const urls = [];
        for (const img of imgs) {
            if (img.src && img.src.startsWith("http") && img.naturalWidth > 100) {
                urls.push(img.src);
            }
        }
        return urls;
    }""")
    return urls or []


async def _fetch_slide_urls(page, total: int) -> list[str | None]:
    """Fetch slide image URLs via in-browser /page_data/ API."""
    base_url = page.url.split("?")[0]
    urls: list[str | None] = []

    for i in range(1, total + 1):
        slide_url = None
        for attempt in range(3):
            result = await page.evaluate(
                """async (args) => {
                    const [base, idx] = args;
                    try {
                        const r = await fetch(base + '/page_data/' + idx);
                        if (!r.ok) return {error: 'HTTP ' + r.status};
                        const text = await r.text();
                        if (!text) return {error: 'empty'};
                        const d = JSON.parse(text);
                        return {url: d.imageUrl || d.directImageUrl || null};
                    } catch(e) { return {error: e.toString()}; }
                }""",
                [base_url, i],
            )
            if isinstance(result, dict) and result.get("url"):
                slide_url = result["url"]
                break
            err = result.get("error", "") if isinstance(result, dict) else ""
            if err.startswith("HTTP 4"):
                break
            if attempt < 2:
                await asyncio.sleep(2)
        urls.append(slide_url)
    return urls


async def _download_images(urls: list[str]) -> list[Image.Image]:
    """Download image URLs and return as RGB PIL Images."""
    LOGGER.info("Downloading %d slide images", len(urls))
    async with httpx.AsyncClient(timeout=30.0) as client:

        async def fetch(img_url: str) -> Image.Image | None:
            try:
                r = await client.get(img_url)
                r.raise_for_status()
                rgba = Image.open(BytesIO(r.content))
                rgb = Image.new("RGB", rgba.size, (255, 255, 255))
                rgb.paste(rgba, mask=rgba.split()[3] if rgba.mode == "RGBA" else None)
                return rgb
            except Exception:
                return None

        results = await asyncio.gather(*[fetch(u) for u in urls])
    images = [img for img in results if img is not None]
    LOGGER.info("Downloaded %d/%d slide images", len(images), len(urls))
    return images


# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------


def _err(error: str, status: str = "error", page_count: int = 0) -> dict:
    return {
        "status": status,
        "error": error,
        "data": None,
        "page_count": page_count,
        "filename": None,
    }


def _client() -> DocsendClient:
    return DocsendClient()
