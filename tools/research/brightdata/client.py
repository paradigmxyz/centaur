"""BrightData REST client — public web search and scraping via api.brightdata.com."""

from __future__ import annotations

import json
import logging
import os
import re
import time
import urllib.parse
from typing import Any

import httpx

from centaur_sdk import secret

_API_HOST = "api.brightdata.com"
_API_BASE_URL = f"https://{_API_HOST}"
_REQUEST_PATH = "/request"
_DISCOVER_PATH = "/discover"
_STATS_PATH = "/zone/bw"

_DEFAULT_TIMEOUT = httpx.Timeout(300.0, connect=30.0)
_MAX_RETRY_AFTER_SECONDS = 5

_DEFAULT_SERP_ZONE = "serp_api"
_DEFAULT_UNLOCKER_ZONE = "unlocker"
_DEFAULT_DISCOVER_POLL_INTERVAL_S = 2.0
_DEFAULT_DISCOVER_TIMEOUT_S = 60.0

# Redacts Bearer tokens and the env-var name from log messages. The token now
# rides in the Authorization header rather than a URL query param, but httpx
# can still log header values at DEBUG, and any exception that stringifies a
# request may surface it — so we filter both shapes defensively.
_BEARER_RE = re.compile(r"Bearer\s+[A-Za-z0-9._\-+/=]+", re.IGNORECASE)
_TOKEN_PARAM_RE = re.compile(r"\btoken=[^&\s\"'>]+", re.IGNORECASE)
_TOKEN_NAME_RE = re.compile(r"BRIGHTDATA_API_TOKEN", re.IGNORECASE)


def _redact(text: str) -> str:
    """Redact bearer tokens, ``token=`` query params, and the env-var name."""
    if not text:
        return text
    text = _BEARER_RE.sub("Bearer [REDACTED]", text)
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


def _build_search_url(query: str, engine: str, cursor: str | None) -> str:
    """Build an engine-specific search URL with ``brd_json=1`` for parsed JSON."""
    q = urllib.parse.quote_plus(query)
    engine_norm = engine.lower()
    if engine_norm == "google":
        url = f"https://www.google.com/search?q={q}&brd_json=1"
        if cursor:
            url += f"&start={urllib.parse.quote_plus(cursor)}"
    elif engine_norm == "bing":
        url = f"https://www.bing.com/search?q={q}&brd_json=1"
        if cursor:
            url += f"&first={urllib.parse.quote_plus(cursor)}"
    elif engine_norm == "yandex":
        url = f"https://yandex.com/search/?text={q}&brd_json=1"
        if cursor:
            url += f"&p={urllib.parse.quote_plus(cursor)}"
    else:
        raise ValueError(f"unsupported search engine: {engine!r}")
    return url


def _parse_body(response: httpx.Response) -> Any:
    """Return parsed JSON when the body is JSON, otherwise wrap as ``{"text": ...}``."""
    text = response.text
    stripped = text.lstrip()
    if stripped.startswith("{") or stripped.startswith("["):
        try:
            return response.json()
        except (json.JSONDecodeError, ValueError):
            pass
    return {"text": text}


def _env_value(name: str) -> str:
    """Return an optional non-secret environment value.

    Do not use ``secret(..., default=...)`` for optional config: Centaur's
    server-mode secret backend intentionally returns stub names for undeclared
    credentials so iron-proxy can replace them on the wire.
    """
    return os.getenv(name, "").strip()  # noqa: TID251 - optional non-secret tool config


class BrightDataClient:
    """Client for BrightData's REST API.

    All outbound calls hit ``api.brightdata.com`` with ``Authorization: Bearer
    <token>``. The tool receives a placeholder string from
    ``centaur_sdk.secret``; iron-proxy swaps in the real value before the
    request leaves the cluster.
    """

    def __init__(
        self,
        api_token: str | None = None,
        serp_zone: str | None = None,
        unlocker_zone: str | None = None,
        transport: httpx.BaseTransport | None = None,
    ):
        self._api_token = api_token
        self._serp_zone = serp_zone
        self._unlocker_zone = unlocker_zone
        self._transport = transport
        self._client: httpx.Client | None = None

    def _get_token(self) -> str:
        if self._api_token:
            return self._api_token
        token = secret("BRIGHTDATA_API_TOKEN", "")
        if not token:
            raise RuntimeError("BRIGHTDATA_API_TOKEN is not configured")
        return token

    def _get_serp_zone(self) -> str:
        return self._serp_zone or _env_value("BRIGHTDATA_SERP_ZONE") or _DEFAULT_SERP_ZONE

    def _get_unlocker_zone(self) -> str:
        return (
            self._unlocker_zone or _env_value("BRIGHTDATA_UNLOCKER_ZONE") or _DEFAULT_UNLOCKER_ZONE
        )

    @property
    def http_client(self) -> httpx.Client:
        if self._client is None:
            kwargs: dict[str, Any] = {
                "base_url": _API_BASE_URL,
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

    def __enter__(self) -> BrightDataClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def _auth_headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self._get_token()}",
            "Content-Type": "application/json",
            "Accept": "application/json, text/plain, */*",
        }

    def _post_request(self, body: dict, *, op: str) -> httpx.Response:
        """POST ``/request`` with bounded 429 retry. ``op`` labels the error message."""
        response = self.http_client.post(
            _REQUEST_PATH,
            json=body,
            headers=self._auth_headers(),
        )
        if response.status_code != 429:
            return response

        retry_after_raw = response.headers.get("retry-after")
        try:
            retry_after = int(retry_after_raw) if retry_after_raw else None
        except ValueError:
            retry_after = None

        if retry_after is None or retry_after <= 0 or retry_after > _MAX_RETRY_AFTER_SECONDS:
            raise RuntimeError(
                f"BrightData rate limited (host={_API_HOST}, op={op}, retry_after={retry_after_raw!r})"
            )

        time.sleep(retry_after)
        retry = self.http_client.post(
            _REQUEST_PATH,
            json=body,
            headers=self._auth_headers(),
        )
        if retry.status_code == 429:
            raise RuntimeError(f"BrightData rate limited after retry (host={_API_HOST}, op={op})")
        return retry

    def _call(self, body: dict, *, op: str) -> Any:
        """POST ``/request`` and parse the response. Wraps transport errors."""
        try:
            response = self._post_request(body, op=op)
        except httpx.RequestError as exc:
            raise RuntimeError(
                f"BrightData request failed (host={_API_HOST}, op={op}): {type(exc).__name__}"
            ) from None

        if response.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"BrightData returned {response.status_code} (host={_API_HOST}, op={op})",
                request=httpx.Request("POST", f"{_API_BASE_URL}{_REQUEST_PATH}"),
                response=response,
            )

        return _parse_body(response)

    def _post_json(self, path: str, body: dict, *, op: str) -> Any:
        try:
            response = self.http_client.post(path, json=body, headers=self._auth_headers())
        except httpx.RequestError as exc:
            raise RuntimeError(
                f"BrightData request failed (host={_API_HOST}, op={op}): {type(exc).__name__}"
            ) from None
        if response.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"BrightData returned {response.status_code} (host={_API_HOST}, op={op})",
                request=httpx.Request("POST", f"{_API_BASE_URL}{path}"),
                response=response,
            )
        return _parse_body(response)

    def _get_json(self, path: str, params: dict[str, Any], *, op: str) -> Any:
        try:
            response = self.http_client.get(path, params=params, headers=self._auth_headers())
        except httpx.RequestError as exc:
            raise RuntimeError(
                f"BrightData request failed (host={_API_HOST}, op={op}): {type(exc).__name__}"
            ) from None
        if response.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"BrightData returned {response.status_code} (host={_API_HOST}, op={op})",
                request=httpx.Request("GET", f"{_API_BASE_URL}{path}"),
                response=response,
            )
        return _parse_body(response)

    # ----- Public methods (these become Centaur tool methods) -----

    def search(self, query: str, engine: str = "google", cursor: str | None = None) -> Any:
        """Run a public web search via BrightData's SERP zone.

        Args:
            query: Search query string.
            engine: ``google``, ``bing``, or ``yandex``.
            cursor: Optional pagination cursor (engine-specific page offset).
        """
        url = _build_search_url(query, engine, cursor)
        body = {"zone": self._get_serp_zone(), "url": url, "format": "json", "method": "GET"}
        return self._call(body, op="search")

    def discover(
        self,
        query: str,
        intent: str | None = None,
        filter_keywords: list[str] | None = None,
        num_results: int = 10,
        result_format: str = "json",
        include_content: bool = False,
        include_images: bool = False,
        mode: str = "standard",
        language: str = "en",
        country: str = "US",
        city: str | None = None,
        start_date: str | None = None,
        end_date: str | None = None,
        poll: bool = True,
        poll_interval_s: float = _DEFAULT_DISCOVER_POLL_INTERVAL_S,
        timeout_s: float = _DEFAULT_DISCOVER_TIMEOUT_S,
    ) -> Any:
        """Run a BrightData Discover search and, by default, wait for results.

        Discover is an async REST API: ``POST /discover`` returns a ``task_id``
        and ``GET /discover?task_id=...`` retrieves ranked results. This wrapper
        polls by default so agents get a usable result in one tool call.
        """
        body: dict[str, Any] = {
            "query": query,
            "num_results": num_results,
            "format": result_format,
            "include_content": include_content,
            "include_images": include_images,
            "mode": mode,
            "language": language,
            "country": country,
        }
        if intent is not None:
            body["intent"] = intent
        if filter_keywords is not None:
            body["filter_keywords"] = filter_keywords
        if city is not None:
            body["city"] = city
        if start_date is not None:
            body["start_date"] = start_date
        if end_date is not None:
            body["end_date"] = end_date

        started = self._post_json(_DISCOVER_PATH, body, op="discover")
        if not poll:
            return started

        task_id = started.get("task_id") if isinstance(started, dict) else None
        if not task_id:
            return started

        deadline = time.monotonic() + max(timeout_s, 0.0)
        while time.monotonic() <= deadline:
            result = self.discover_result(str(task_id))
            if not isinstance(result, dict) or result.get("status") != "processing":
                return result
            time.sleep(max(poll_interval_s, 0.0))
        raise TimeoutError(f"BrightData discover timed out (host={_API_HOST}, task_id={task_id})")

    def discover_result(self, task_id: str) -> Any:
        """Fetch results for a BrightData Discover ``task_id``."""
        return self._get_json(_DISCOVER_PATH, {"task_id": task_id}, op="discover_result")

    def scrape_markdown(self, url: str) -> Any:
        """Fetch a public page via the Web Unlocker zone, rendered as Markdown."""
        body = {
            "zone": self._get_unlocker_zone(),
            "url": url,
            "format": "raw",
            "data_format": "markdown",
        }
        return self._call(body, op="scrape_markdown")

    def scrape_html(self, url: str) -> Any:
        """Fetch a public page via the Web Unlocker zone, returned as HTML."""
        body = {"zone": self._get_unlocker_zone(), "url": url, "format": "raw"}
        return self._call(body, op="scrape_html")

    def session_stats(self) -> Any:
        """Return bandwidth stats for the configured SERP and Unlocker zones."""
        stats: dict[str, Any] = {}
        for label, zone in (
            ("serp", self._get_serp_zone()),
            ("unlocker", self._get_unlocker_zone()),
        ):
            try:
                response = self.http_client.get(
                    _STATS_PATH,
                    params={"zone": zone},
                    headers=self._auth_headers(),
                )
            except httpx.RequestError as exc:
                raise RuntimeError(
                    f"BrightData stats request failed (host={_API_HOST}, zone={zone}): {type(exc).__name__}"
                ) from None
            if response.status_code >= 400:
                raise httpx.HTTPStatusError(
                    f"BrightData stats returned {response.status_code} (host={_API_HOST}, zone={zone})",
                    request=httpx.Request("GET", f"{_API_BASE_URL}{_STATS_PATH}"),
                    response=response,
                )
            stats[label] = {"zone": zone, "data": _parse_body(response)}
        return stats


def _client() -> BrightDataClient:
    return BrightDataClient()
