"""BrightData MCP client — public web search, discovery, and scraping."""

from __future__ import annotations

import contextlib
import json
import logging
import re
import time
import uuid
from typing import Any

import httpx

from centaur_sdk import secret


_MCP_HOST = "mcp.brightdata.com"
_MCP_BASE_URL = f"https://{_MCP_HOST}"
_MCP_PATH = "/mcp"
_MCP_PROTOCOL_VERSION = "2024-11-05"
_SESSION_HEADER = "Mcp-Session-Id"

_DEFAULT_TIMEOUT = httpx.Timeout(300.0, connect=30.0)
_MAX_RETRY_AFTER_SECONDS = 5

# Redacts `token=...` and `BRIGHTDATA_API_TOKEN` substrings from log messages
# emitted by httpx/httpcore/this module — the token sits in the URL query and
# httpx logs full URLs at DEBUG level.
_TOKEN_PARAM_RE = re.compile(r"\btoken=[^&\s\"'>]+", re.IGNORECASE)
_TOKEN_NAME_RE = re.compile(r"BRIGHTDATA_API_TOKEN", re.IGNORECASE)


def _redact(text: str) -> str:
    """Redact token-bearing URLs and the env-var name from arbitrary strings."""
    if not text:
        return text
    text = _TOKEN_PARAM_RE.sub("[REDACTED]", text)
    text = _TOKEN_NAME_RE.sub("[REDACTED]", text)
    return text


def _redact_arg(value: Any) -> Any:
    """Redact strings (and ``httpx.URL`` instances) while preserving other types.

    httpx logs status codes as ``int`` against ``%d`` format specifiers — we must
    not stringify those, or ``msg % args`` will raise ``TypeError``.
    """
    if isinstance(value, str):
        return _redact(value)
    # httpx.URL / httpx.Request / httpx.Response render with their __str__/__repr__
    # in log records; only redact those known leaky types.
    if isinstance(value, (httpx.URL, httpx.Request, httpx.Response)):
        return _redact(str(value))
    return value


class _RedactTokenFilter(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        if isinstance(record.msg, str):
            record.msg = _redact(record.msg)
        if record.args:
            try:
                if isinstance(record.args, tuple):
                    record.args = tuple(_redact_arg(a) for a in record.args)
                elif isinstance(record.args, dict):
                    record.args = {k: _redact_arg(v) for k, v in record.args.items()}
            except Exception:
                pass
        return True


_REDACTION_INSTALLED = False


def _install_redaction_once() -> None:
    global _REDACTION_INSTALLED
    if _REDACTION_INSTALLED:
        return
    flt = _RedactTokenFilter()
    for name in ("httpx", "httpcore", "httpcore.http11", "httpcore.connection", __name__):
        logger = logging.getLogger(name)
        if not any(isinstance(f, _RedactTokenFilter) for f in logger.filters):
            logger.addFilter(flt)
    _REDACTION_INSTALLED = True


_install_redaction_once()


def _parse_sse_events(response_text: str) -> list[dict]:
    """Parse SSE response text into JSON events. Handles multi-line data blocks."""
    events: list[dict] = []
    buf: list[str] = []

    for raw in response_text.splitlines():
        line = raw.rstrip("\r")
        if line.startswith("data:"):
            buf.append(line[len("data:") :].lstrip())
            continue
        if line == "" and buf:
            payload = "\n".join(buf)
            buf = []
            with contextlib.suppress(json.JSONDecodeError):
                events.append(json.loads(payload))

    if buf:
        payload = "\n".join(buf)
        with contextlib.suppress(json.JSONDecodeError):
            events.append(json.loads(payload))

    return events


def _extract_mcp_payload(envelope: dict) -> Any:
    """Pull the user-facing result out of an MCP envelope.

    Order: ``structuredContent`` → JSON inside ``content[*].text`` → raw text.
    """
    result = envelope.get("result", {}) or {}

    structured = result.get("structuredContent")
    if structured is not None:
        return structured

    content = result.get("content")
    if isinstance(content, list) and content:
        first = content[0]
        if isinstance(first, dict) and "text" in first:
            text = first["text"]
            try:
                return json.loads(text)
            except (json.JSONDecodeError, TypeError):
                return {"text": text}
    if isinstance(content, dict) and "text" in content:
        text = content["text"]
        try:
            return json.loads(text)
        except (json.JSONDecodeError, TypeError):
            return {"text": text}

    return content


class BrightDataClient:
    """Client for BrightData's hosted MCP server.

    All outbound calls hit ``mcp.brightdata.com`` and carry the BrightData
    API token in the URL query (``?token=...``). The tool receives a
    placeholder string from ``centaur_sdk.secret``; iron-proxy swaps in the
    real value before the request leaves the cluster.
    """

    def __init__(self, api_token: str | None = None, transport: httpx.BaseTransport | None = None):
        self._api_token = api_token
        self._transport = transport
        self._client: httpx.Client | None = None
        self._session_id: str | None = None

    def _get_token(self) -> str:
        if self._api_token:
            return self._api_token
        token = secret("BRIGHTDATA_API_TOKEN", "")
        if not token:
            raise RuntimeError("BRIGHTDATA_API_TOKEN is not configured")
        return token

    @property
    def http_client(self) -> httpx.Client:
        if self._client is None:
            kwargs: dict[str, Any] = {
                "base_url": _MCP_BASE_URL,
                "headers": {
                    "Content-Type": "application/json",
                    "Accept": "application/json, text/event-stream",
                },
                "timeout": _DEFAULT_TIMEOUT,
            }
            if self._transport is not None:
                kwargs["transport"] = self._transport
            self._client = httpx.Client(**kwargs)
        return self._client

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> "BrightDataClient":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def _post_mcp(self, payload: dict, *, with_session: bool = True) -> httpx.Response:
        headers: dict[str, str] = {}
        if with_session and self._session_id:
            headers[_SESSION_HEADER] = self._session_id
        return self.http_client.post(
            _MCP_PATH,
            json=payload,
            params={"token": self._get_token()},
            headers=headers or None,
        )

    def _initialize_session(self) -> None:
        """Run the MCP Streamable-HTTP handshake — ``initialize`` then ``notifications/initialized``."""
        init_payload = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "initialize",
            "params": {
                "protocolVersion": _MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "centaur-brightdata", "version": "0.1.0"},
            },
        }
        response = self._post_mcp(init_payload, with_session=False)
        if response.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"BrightData MCP initialize returned {response.status_code} (host={_MCP_HOST})",
                request=httpx.Request("POST", f"{_MCP_BASE_URL}{_MCP_PATH}"),
                response=response,
            )
        session_id = response.headers.get(_SESSION_HEADER)
        if not session_id:
            raise RuntimeError(
                f"BrightData MCP did not return a session id (host={_MCP_HOST})"
            )
        self._session_id = session_id

        ack = {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        }
        self._post_mcp(ack)

    @staticmethod
    def _looks_like_session_expired(response: httpx.Response) -> bool:
        """Detect BrightData's stale-session signals.

        BrightData returns 404 (or 400 with "session"-shaped text) when the
        cached ``Mcp-Session-Id`` has expired or been evicted server-side.
        """
        if response.status_code not in (400, 404):
            return False
        try:
            text = response.text.lower()
        except Exception:
            return response.status_code == 404
        return ("session" in text) or (response.status_code == 404)

    def _request_with_rate_limit(self, payload: dict) -> httpx.Response:
        """POST once with bounded retries: 429 → retry once after Retry-After;
        stale session (4xx + session signal) → re-handshake and retry once."""
        if self._session_id is None:
            self._initialize_session()
        response = self._post_mcp(payload)

        if self._looks_like_session_expired(response):
            self._session_id = None
            self._initialize_session()
            response = self._post_mcp(payload)

        if response.status_code != 429:
            return response

        retry_after_raw = response.headers.get("retry-after")
        try:
            retry_after = int(retry_after_raw) if retry_after_raw else None
        except ValueError:
            retry_after = None

        if retry_after is None or retry_after <= 0 or retry_after > _MAX_RETRY_AFTER_SECONDS:
            raise RuntimeError(
                f"BrightData MCP rate limited (host={_MCP_HOST}, retry_after={retry_after_raw!r})"
            )

        time.sleep(retry_after)
        retry = self._post_mcp(payload)
        if retry.status_code == 429:
            raise RuntimeError(
                f"BrightData MCP rate limited after retry (host={_MCP_HOST})"
            )
        return retry

    def _mcp_call(self, tool_name: str, arguments: dict) -> Any:
        """Invoke an MCP tool via JSON-RPC ``tools/call``.

        Raises ``RuntimeError`` on MCP-level errors and ``httpx.HTTPStatusError``
        on non-2xx HTTP responses (except 429, which goes through bounded retry).
        All exception messages identify the host and MCP tool name only — never
        the request URL — to avoid token leaks.
        """
        payload = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": arguments},
        }

        try:
            response = self._request_with_rate_limit(payload)
        except httpx.RequestError as exc:
            raise RuntimeError(
                f"BrightData MCP request failed (host={_MCP_HOST}, tool={tool_name}): {type(exc).__name__}"
            ) from None

        if response.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"BrightData MCP returned {response.status_code} (host={_MCP_HOST}, tool={tool_name})",
                request=httpx.Request("POST", f"{_MCP_BASE_URL}{_MCP_PATH}"),
                response=response,
            )

        body_text = response.text
        content_type = response.headers.get("content-type", "").lower()

        envelope: dict = {}
        if "text/event-stream" in content_type or body_text.lstrip().startswith("data:"):
            events = _parse_sse_events(body_text)
            for event in reversed(events):
                if isinstance(event, dict) and ("result" in event or "error" in event):
                    envelope = event
                    break
            if not envelope and events:
                envelope = events[-1] if isinstance(events[-1], dict) else {}
        elif body_text.lstrip().startswith("{"):
            try:
                envelope = response.json()
            except (json.JSONDecodeError, ValueError):
                envelope = {}

        if "error" in envelope:
            err = envelope["error"]
            message = err.get("message") if isinstance(err, dict) else str(err)
            raise RuntimeError(
                f"BrightData MCP error (tool={tool_name}): {message}"
            )

        return _extract_mcp_payload(envelope)

    # ----- Public methods (these become Centaur tool methods) -----

    def search(self, query: str, engine: str = "google", cursor: str | None = None) -> Any:
        """Run a public web search.

        Args:
            query: Search query string.
            engine: ``google``, ``bing``, or ``yandex``.
            cursor: Optional pagination cursor returned by a previous call.
        """
        args: dict[str, Any] = {"query": query, "engine": engine}
        if cursor is not None:
            args["cursor"] = cursor
        return self._mcp_call("search_engine", args)

    def discover(self, query: str) -> Any:
        """Discover relevant URLs across the public web for the given query."""
        return self._mcp_call("discover", {"query": query})

    def scrape_markdown(self, url: str) -> Any:
        """Fetch a public page and return it rendered as Markdown."""
        return self._mcp_call("scrape_as_markdown", {"url": url})

    def scrape_html(self, url: str) -> Any:
        """Fetch a public page and return the rendered HTML."""
        return self._mcp_call("scrape_as_html", {"url": url})

    def session_stats(self) -> Any:
        """Return BrightData session usage statistics."""
        return self._mcp_call("session_stats", {})


def _client() -> BrightDataClient:
    return BrightDataClient()
