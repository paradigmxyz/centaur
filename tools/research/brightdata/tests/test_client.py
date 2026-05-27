"""Unit tests for the BrightData REST client.

Layout & import path follow the existing ``tools/research/youtube/test_client.py``
convention — we insert the repo root on ``sys.path`` so the test file can be
discovered both by ``uv run pytest tools/research/brightdata/tests/test_client.py``
and from inside the API service test environment.
"""

from __future__ import annotations

import json
import logging
import sys
from pathlib import Path
from typing import Callable

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.research.brightdata.client import (  # noqa: E402
    BrightDataClient,
    _build_search_url,
    _client as client_factory,
    _parse_body,
    _redact,
)


_FAKE_TOKEN = "fake-token-XYZ123"
_FAKE_SERP_ZONE = "test_serp"
_FAKE_UNLOCKER_ZONE = "test_unlocker"


def _make_client(handler: Callable[[httpx.Request], httpx.Response]) -> BrightDataClient:
    transport = httpx.MockTransport(handler)
    return BrightDataClient(
        api_token=_FAKE_TOKEN,
        serp_zone=_FAKE_SERP_ZONE,
        unlocker_zone=_FAKE_UNLOCKER_ZONE,
        transport=transport,
    )


# ---------- search URL builder ----------


def test_build_search_url_google() -> None:
    url = _build_search_url("anthropic claude", "google", None)
    assert url == "https://www.google.com/search?q=anthropic+claude&brd_json=1"


def test_build_search_url_google_with_cursor() -> None:
    url = _build_search_url("claude", "google", "20")
    assert url == "https://www.google.com/search?q=claude&brd_json=1&start=20"


def test_build_search_url_bing() -> None:
    url = _build_search_url("claude", "bing", None)
    assert url == "https://www.bing.com/search?q=claude&brd_json=1"


def test_build_search_url_yandex() -> None:
    url = _build_search_url("claude", "yandex", None)
    assert url == "https://yandex.com/search/?text=claude&brd_json=1"


def test_build_search_url_unsupported_engine_raises() -> None:
    with pytest.raises(ValueError, match="unsupported search engine"):
        _build_search_url("q", "duckduckgo", None)


# ---------- body parser ----------


def test_parse_body_returns_json_when_object() -> None:
    response = httpx.Response(200, request=httpx.Request("GET", "https://x"),
                              json={"hits": [1, 2]})
    assert _parse_body(response) == {"hits": [1, 2]}


def test_parse_body_returns_json_when_array() -> None:
    response = httpx.Response(200, request=httpx.Request("GET", "https://x"),
                              json=[{"a": 1}, {"b": 2}])
    assert _parse_body(response) == [{"a": 1}, {"b": 2}]


def test_parse_body_wraps_text_when_not_json() -> None:
    response = httpx.Response(200, request=httpx.Request("GET", "https://x"),
                              text="# markdown body")
    assert _parse_body(response) == {"text": "# markdown body"}


def test_parse_body_wraps_invalid_json_as_text() -> None:
    response = httpx.Response(200, request=httpx.Request("GET", "https://x"),
                              content=b"{not valid json")
    assert _parse_body(response) == {"text": "{not valid json"}


# ---------- request shape: search ----------


def _captured_handler(
    response_body: dict | str | None = None,
) -> tuple[dict, Callable[[httpx.Request], httpx.Response]]:
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["method"] = request.method
        captured["path"] = request.url.path
        captured["query"] = dict(request.url.params)
        captured["headers"] = dict(request.headers)
        captured["body"] = json.loads(request.content) if request.content else None
        if response_body is None:
            return httpx.Response(200, request=request, json={"ok": True})
        if isinstance(response_body, str):
            return httpx.Response(200, request=request, text=response_body)
        return httpx.Response(200, request=request, json=response_body)

    return captured, handler


def test_search_posts_to_request_with_serp_zone() -> None:
    captured, handler = _captured_handler({"organic": []})
    c = _make_client(handler)
    c.search("anthropic")
    assert captured["method"] == "POST"
    assert captured["path"] == "/request"
    body = captured["body"]
    assert body["zone"] == _FAKE_SERP_ZONE
    assert body["format"] == "raw"
    assert body["url"] == "https://www.google.com/search?q=anthropic&brd_json=1"


def test_search_passes_cursor_into_url() -> None:
    captured, handler = _captured_handler({"organic": []})
    c = _make_client(handler)
    c.search("claude", engine="bing", cursor="11")
    assert captured["body"]["url"] == "https://www.bing.com/search?q=claude&brd_json=1&first=11"


def test_search_sends_bearer_auth_header() -> None:
    captured, handler = _captured_handler({"organic": []})
    c = _make_client(handler)
    c.search("anthropic")
    assert captured["headers"]["authorization"] == f"Bearer {_FAKE_TOKEN}"


def test_search_does_not_put_token_in_query() -> None:
    captured, handler = _captured_handler({"organic": []})
    c = _make_client(handler)
    c.search("anthropic")
    assert "token" not in captured["query"]
    assert _FAKE_TOKEN not in str(captured["query"])


def test_search_returns_parsed_json() -> None:
    _, handler = _captured_handler({"organic": [{"title": "x"}]})
    c = _make_client(handler)
    assert c.search("q") == {"organic": [{"title": "x"}]}


# ---------- request shape: scrape ----------


def test_scrape_markdown_uses_unlocker_zone_and_data_format() -> None:
    captured, handler = _captured_handler("# heading")
    c = _make_client(handler)
    result = c.scrape_markdown("https://example.com")
    body = captured["body"]
    assert body["zone"] == _FAKE_UNLOCKER_ZONE
    assert body["url"] == "https://example.com"
    assert body["format"] == "raw"
    assert body["data_format"] == "markdown"
    assert result == {"text": "# heading"}


def test_scrape_html_uses_unlocker_zone_without_data_format() -> None:
    captured, handler = _captured_handler("<html><body>hi</body></html>")
    c = _make_client(handler)
    result = c.scrape_html("https://example.com")
    body = captured["body"]
    assert body["zone"] == _FAKE_UNLOCKER_ZONE
    assert body["url"] == "https://example.com"
    assert body["format"] == "raw"
    assert "data_format" not in body
    assert result == {"text": "<html><body>hi</body></html>"}


# ---------- request shape: session_stats ----------


def test_session_stats_queries_both_zones() -> None:
    seen: list[dict] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append({
            "method": request.method,
            "path": request.url.path,
            "zone": request.url.params.get("zone"),
            "auth": request.headers.get("authorization"),
        })
        return httpx.Response(200, request=request, json={"requests": 7})

    c = _make_client(handler)
    stats = c.session_stats()
    assert len(seen) == 2
    assert {s["zone"] for s in seen} == {_FAKE_SERP_ZONE, _FAKE_UNLOCKER_ZONE}
    for s in seen:
        assert s["method"] == "GET"
        assert s["path"] == "/zone/statistic"
        assert s["auth"] == f"Bearer {_FAKE_TOKEN}"
    assert stats["serp"]["zone"] == _FAKE_SERP_ZONE
    assert stats["unlocker"]["zone"] == _FAKE_UNLOCKER_ZONE
    assert stats["serp"]["data"] == {"requests": 7}


# ---------- error semantics ----------


def test_http_500_raises_http_status_error() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(500, request=request, text="boom")

    c = _make_client(handler)
    with pytest.raises(httpx.HTTPStatusError):
        c.search("q")


def test_http_403_raises_http_status_error() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(403, request=request, text="forbidden")

    c = _make_client(handler)
    with pytest.raises(httpx.HTTPStatusError):
        c.scrape_html("https://example.com")


def test_request_error_wrapped_into_runtime() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("dns failure")

    c = _make_client(handler)
    with pytest.raises(RuntimeError, match="ConnectError"):
        c.search("q")


# ---------- 429 bounded retry ----------


def test_429_with_small_retry_after_retries_once(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("tools.research.brightdata.client.time.sleep", lambda _s: None)
    calls = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        calls["n"] += 1
        if calls["n"] == 1:
            return httpx.Response(
                429, request=request, headers={"retry-after": "1"}, text="rate limit"
            )
        return httpx.Response(200, request=request, json={"ok": True})

    c = _make_client(handler)
    assert c.search("q") == {"ok": True}
    assert calls["n"] == 2


def test_429_without_retry_after_raises_immediately() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(429, request=request, text="rate limit")

    c = _make_client(handler)
    with pytest.raises(RuntimeError, match="rate limited"):
        c.search("q")


def test_429_with_large_retry_after_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("tools.research.brightdata.client.time.sleep", lambda _s: None)

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(429, request=request, headers={"retry-after": "120"})

    c = _make_client(handler)
    with pytest.raises(RuntimeError, match="rate limited"):
        c.search("q")


def test_429_retry_still_429_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("tools.research.brightdata.client.time.sleep", lambda _s: None)

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(429, request=request, headers={"retry-after": "1"})

    c = _make_client(handler)
    with pytest.raises(RuntimeError, match="after retry"):
        c.search("q")


# ---------- attack-surface guarantees ----------


def test_no_mcp_passthrough_or_raw_call() -> None:
    """Tool must NOT expose raw MCP / passthrough callers."""
    c = BrightDataClient()
    assert not hasattr(c, "call_tool")
    assert not hasattr(c, "raw_mcp")
    assert not hasattr(c, "discover")  # dropped — no direct REST equivalent
    public_methods = {
        name for name in vars(BrightDataClient) if not name.startswith("_")
    }
    public_methods.discard("close")
    public_methods.discard("http_client")
    assert public_methods == {
        "search", "scrape_markdown", "scrape_html", "session_stats",
    }


# ---------- zone defaults ----------


def test_zone_falls_back_to_default_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        "tools.research.brightdata.client.secret", lambda _name, _default: ""
    )
    captured, handler = _captured_handler({"ok": True})
    transport = httpx.MockTransport(handler)
    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=transport)
    c.search("q")
    assert captured["body"]["zone"] == "serp_api"

    captured2, handler2 = _captured_handler("hi")
    transport2 = httpx.MockTransport(handler2)
    c2 = BrightDataClient(api_token=_FAKE_TOKEN, transport=transport2)
    c2.scrape_markdown("https://example.com")
    assert captured2["body"]["zone"] == "unlocker"


# ---------- token redaction: exceptions ----------


def test_exception_message_does_not_contain_token() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(500, request=request, text="upstream went away")

    c = _make_client(handler)
    try:
        c.search("q")
    except httpx.HTTPStatusError as exc:
        text = repr(exc) + str(exc)
        assert _FAKE_TOKEN not in text
        assert "Bearer " + _FAKE_TOKEN not in text
        assert "BRIGHTDATA_API_TOKEN" not in text
    else:
        pytest.fail("expected HTTPStatusError")


def test_request_error_message_does_not_contain_token() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError(
            f"could not connect; Authorization: Bearer {_FAKE_TOKEN}"
        )

    c = _make_client(handler)
    try:
        c.search("q")
    except RuntimeError as exc:
        text = repr(exc) + str(exc)
        assert _FAKE_TOKEN not in text


# ---------- token redaction: logs at DEBUG ----------


def _assert_clean(records: list[logging.LogRecord]) -> None:
    for r in records:
        msg = r.getMessage()
        assert _FAKE_TOKEN not in msg, f"token leaked in log: {msg!r}"
        assert "BRIGHTDATA_API_TOKEN" not in msg, f"env-var name leaked: {msg!r}"
        assert f"Bearer {_FAKE_TOKEN}" not in msg, f"Bearer token leaked: {msg!r}"


@pytest.fixture
def debug_loggers(caplog: pytest.LogCaptureFixture) -> pytest.LogCaptureFixture:
    caplog.set_level(logging.DEBUG, logger="httpx")
    caplog.set_level(logging.DEBUG, logger="httpcore")
    caplog.set_level(logging.DEBUG, logger="httpcore.http11")
    caplog.set_level(logging.DEBUG, logger="tools.research.brightdata.client")
    return caplog


def test_logs_redact_token_on_success(debug_loggers: pytest.LogCaptureFixture) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        logging.getLogger("tools.research.brightdata.client").debug(
            "outbound request with header %s", request.headers.get("authorization")
        )
        return httpx.Response(200, request=request, json={"ok": True})

    c = _make_client(handler)
    c.search("q")
    _assert_clean(debug_loggers.records)


def test_logs_redact_token_on_http_error(debug_loggers: pytest.LogCaptureFixture) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        logging.getLogger("tools.research.brightdata.client").debug(
            "outbound failing request with header %s", request.headers.get("authorization")
        )
        return httpx.Response(500, request=request, text="boom")

    c = _make_client(handler)
    with pytest.raises(httpx.HTTPStatusError):
        c.search("q")
    _assert_clean(debug_loggers.records)


def test_logs_redact_token_on_429(
    debug_loggers: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("tools.research.brightdata.client.time.sleep", lambda _s: None)

    def handler(request: httpx.Request) -> httpx.Response:
        logging.getLogger("tools.research.brightdata.client").debug(
            "rate limited request with header %s", request.headers.get("authorization")
        )
        return httpx.Response(429, request=request, headers={"retry-after": "120"})

    c = _make_client(handler)
    with pytest.raises(RuntimeError):
        c.search("q")
    _assert_clean(debug_loggers.records)


# ---------- centaur tool discovery contract ----------


def test_module_exposes_client_factory() -> None:
    instance = client_factory()
    assert isinstance(instance, BrightDataClient)


def test_redact_helper_handles_both_patterns() -> None:
    assert "[REDACTED]" in _redact("Authorization: Bearer abc123def")
    assert "abc123def" not in _redact("Authorization: Bearer abc123def")
    assert "[REDACTED]" in _redact("see https://x/y?token=abc&z=1")
    assert "abc" not in _redact("see https://x/y?token=abc&z=1").split("&")[0]
    assert "BRIGHTDATA_API_TOKEN" not in _redact("missing BRIGHTDATA_API_TOKEN env var")
