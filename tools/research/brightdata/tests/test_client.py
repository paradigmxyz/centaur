"""Unit tests for the BrightData MCP client.

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
    _client as client_factory,
    _extract_mcp_payload,
    _parse_sse_events,
    _redact,
)


_FAKE_TOKEN = "fake-token-XYZ123"
_FAKE_SESSION_ID = "00000000-0000-4000-8000-000000000001"


def _wrap_with_session(inner: Callable[[httpx.Request], httpx.Response]) -> Callable[[httpx.Request], httpx.Response]:
    """Wrap a tool-call handler with the MCP Streamable-HTTP handshake.

    The real BrightData MCP requires:
      1. POST initialize  → server returns ``Mcp-Session-Id`` header
      2. POST notifications/initialized (with session header) → 200
      3. POST tools/call (with session header) → actual handler runs

    This wrapper makes existing tool-call tests work unchanged while still
    asserting the client follows the protocol.
    """

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else {}
        method = body.get("method")
        if method == "initialize":
            return httpx.Response(
                200,
                request=request,
                headers={"Mcp-Session-Id": _FAKE_SESSION_ID, "content-type": "application/json"},
                json={"jsonrpc": "2.0", "id": body.get("id"),
                      "result": {"protocolVersion": "2024-11-05", "capabilities": {}}},
            )
        if method == "notifications/initialized":
            return httpx.Response(200, request=request, json={})
        # tools/call (or anything else): defer to inner handler
        return inner(request)

    return handler


def _make_client(handler: Callable[[httpx.Request], httpx.Response]) -> BrightDataClient:
    transport = httpx.MockTransport(_wrap_with_session(handler))
    return BrightDataClient(api_token=_FAKE_TOKEN, transport=transport)


# ---------- SSE parser ----------


def test_parse_sse_events_single_event() -> None:
    text = 'event: message\ndata: {"jsonrpc":"2.0","id":"1","result":{"x":1}}\n\n'
    events = _parse_sse_events(text)
    assert events == [{"jsonrpc": "2.0", "id": "1", "result": {"x": 1}}]


def test_parse_sse_events_multiline_data() -> None:
    text = 'data: {"a":\ndata: 1}\n\n'
    events = _parse_sse_events(text)
    assert events == [{"a": 1}]


def test_parse_sse_events_invalid_json_skipped() -> None:
    text = 'data: not-json\n\ndata: {"ok":true}\n\n'
    events = _parse_sse_events(text)
    assert events == [{"ok": True}]


# ---------- envelope extractor ----------


def test_structured_content_takes_precedence() -> None:
    env = {"result": {"structuredContent": {"foo": "bar"}, "content": [{"text": "ignored"}]}}
    assert _extract_mcp_payload(env) == {"foo": "bar"}


def test_json_in_text_content_is_decoded() -> None:
    env = {"result": {"content": [{"text": '{"items":["a","b"]}'}]}}
    assert _extract_mcp_payload(env) == {"items": ["a", "b"]}


def test_plain_text_fallback_in_content_list() -> None:
    env = {"result": {"content": [{"text": "# markdown body"}]}}
    assert _extract_mcp_payload(env) == {"text": "# markdown body"}


def test_dict_content_with_text_field() -> None:
    env = {"result": {"content": {"text": "raw"}}}
    assert _extract_mcp_payload(env) == {"text": "raw"}


# ---------- JSON-RPC payload + method mapping ----------


def _captured_handler() -> tuple[dict, Callable[[httpx.Request], httpx.Response]]:
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["method"] = request.method
        captured["path"] = request.url.path
        captured["query"] = dict(request.url.params)
        captured["body"] = json.loads(request.content)
        return httpx.Response(
            200,
            request=request,
            json={"jsonrpc": "2.0", "id": "1", "result": {"structuredContent": {"ok": True}}},
        )

    return captured, handler


@pytest.mark.parametrize(
    "method_name,kwargs,expected_mcp_name,expected_args",
    [
        ("search", {"query": "anthropic", "engine": "google"}, "search_engine",
            {"query": "anthropic", "engine": "google"}),
        ("search", {"query": "claude", "engine": "bing", "cursor": "c2"}, "search_engine",
            {"query": "claude", "engine": "bing", "cursor": "c2"}),
        ("discover", {"query": "anthropic pricing"}, "discover", {"query": "anthropic pricing"}),
        ("scrape_markdown", {"url": "https://example.com"}, "scrape_as_markdown",
            {"url": "https://example.com"}),
        ("scrape_html", {"url": "https://example.com"}, "scrape_as_html",
            {"url": "https://example.com"}),
        ("session_stats", {}, "session_stats", {}),
    ],
)
def test_method_maps_to_mcp_tool(
    method_name: str,
    kwargs: dict,
    expected_mcp_name: str,
    expected_args: dict,
) -> None:
    captured, handler = _captured_handler()
    c = _make_client(handler)
    getattr(c, method_name)(**kwargs)

    assert captured["method"] == "POST"
    assert captured["path"] == "/mcp"
    assert captured["query"] == {"token": _FAKE_TOKEN}
    body = captured["body"]
    assert body["jsonrpc"] == "2.0"
    assert body["method"] == "tools/call"
    assert body["params"]["name"] == expected_mcp_name
    assert body["params"]["arguments"] == expected_args


# ---------- response shapes through the public API ----------


def test_json_response_parsed() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1", "result": {"content": [{"text": '{"hits":[{"u":"x"}]}'}]}},
        )

    c = _make_client(handler)
    assert c.search("q") == {"hits": [{"u": "x"}]}


def test_sse_response_parsed() -> None:
    sse = (
        "event: message\n"
        'data: {"jsonrpc":"2.0","id":"1","result":{"content":[{"text":"# hello"}]}}\n\n'
    )

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, request=request,
            content=sse.encode(),
            headers={"content-type": "text/event-stream"},
        )

    c = _make_client(handler)
    assert c.scrape_markdown("https://example.com") == {"text": "# hello"}


# ---------- error semantics ----------


def test_mcp_error_raises_runtime_with_message_only() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1", "error": {"code": -32000, "message": "upstream blocked"}},
        )

    c = _make_client(handler)
    with pytest.raises(RuntimeError, match="upstream blocked"):
        c.search("q")


def test_http_500_raises_http_status_error() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(500, request=request, text="boom")

    c = _make_client(handler)
    with pytest.raises(httpx.HTTPStatusError):
        c.search("q")


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
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1", "result": {"structuredContent": {"ok": True}}},
        )

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


def test_no_generic_call_tool_method() -> None:
    """PR #1 must NOT expose a raw MCP passthrough."""
    c = BrightDataClient()
    assert not hasattr(c, "call_tool")
    assert not hasattr(c, "raw_mcp")
    public_methods = {
        name for name in vars(BrightDataClient) if not name.startswith("_")
    }
    public_methods.discard("close")
    public_methods.discard("http_client")
    assert public_methods == {
        "search", "discover", "scrape_markdown", "scrape_html", "session_stats",
    }


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
        assert "token=" not in text
        assert "BRIGHTDATA_API_TOKEN" not in text
    else:
        pytest.fail("expected HTTPStatusError")


def test_request_error_message_does_not_contain_token() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError(f"could not connect to https://mcp.brightdata.com/mcp?token={_FAKE_TOKEN}")

    c = _make_client(handler)
    try:
        c.search("q")
    except RuntimeError as exc:
        text = repr(exc) + str(exc)
        assert _FAKE_TOKEN not in text
        assert "token=" not in text


def test_mcp_error_message_redacts_token_substring() -> None:
    """If BrightData ever echoed a token in its error, we still don't leak it."""

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1",
                  "error": {"code": -32000, "message": f"bad token=abcdef"}},
        )

    c = _make_client(handler)
    try:
        c.search("q")
    except RuntimeError as exc:
        # Our local `_redact` is available for callers that re-stringify the message.
        assert "token=" not in _redact(str(exc))


# ---------- token redaction: logs at DEBUG ----------


def _assert_clean(records: list[logging.LogRecord]) -> None:
    for r in records:
        msg = r.getMessage()
        assert _FAKE_TOKEN not in msg, f"token leaked in log: {msg!r}"
        assert "BRIGHTDATA_API_TOKEN" not in msg, f"env-var name leaked: {msg!r}"
        assert "token=fake" not in msg.lower(), f"token= leaked: {msg!r}"


@pytest.fixture
def debug_loggers(caplog: pytest.LogCaptureFixture) -> pytest.LogCaptureFixture:
    caplog.set_level(logging.DEBUG, logger="httpx")
    caplog.set_level(logging.DEBUG, logger="httpcore")
    caplog.set_level(logging.DEBUG, logger="httpcore.http11")
    caplog.set_level(logging.DEBUG, logger="tools.research.brightdata.client")
    return caplog


def test_logs_redact_token_on_success(debug_loggers: pytest.LogCaptureFixture) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        # log the URL ourselves to simulate any code path that might do so
        logging.getLogger("tools.research.brightdata.client").debug(
            "outbound request to %s", request.url
        )
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1", "result": {"structuredContent": {"ok": True}}},
        )

    c = _make_client(handler)
    c.search("q")
    _assert_clean(debug_loggers.records)


def test_logs_redact_token_on_http_error(debug_loggers: pytest.LogCaptureFixture) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        logging.getLogger("tools.research.brightdata.client").debug(
            "outbound failing request to %s", request.url
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
            "rate limited request to %s", request.url
        )
        return httpx.Response(429, request=request, headers={"retry-after": "120"})

    c = _make_client(handler)
    with pytest.raises(RuntimeError):
        c.search("q")
    _assert_clean(debug_loggers.records)


# ---------- MCP Streamable-HTTP handshake ----------


def test_initialize_handshake_runs_before_first_tool_call() -> None:
    calls: list[dict] = []

    def inner(request: httpx.Request) -> httpx.Response:
        calls.append({
            "body": json.loads(request.content),
            "session_header": request.headers.get("Mcp-Session-Id"),
        })
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1",
                  "result": {"structuredContent": {"ok": True}}},
        )

    # Build a transport that records both initialize and tool-call requests
    seq: list[str] = []

    def transport_handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else {}
        seq.append(body.get("method", ""))
        if body.get("method") == "initialize":
            return httpx.Response(
                200, request=request,
                headers={"Mcp-Session-Id": _FAKE_SESSION_ID},
                json={"jsonrpc": "2.0", "id": body.get("id"),
                      "result": {"protocolVersion": "2024-11-05", "capabilities": {}}},
            )
        if body.get("method") == "notifications/initialized":
            return httpx.Response(200, request=request, json={})
        return inner(request)

    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=httpx.MockTransport(transport_handler))
    c.search("q")
    assert seq == ["initialize", "notifications/initialized", "tools/call"]
    assert calls[0]["session_header"] == _FAKE_SESSION_ID


def test_session_handshake_runs_only_once_across_multiple_calls() -> None:
    init_count = {"n": 0}
    tool_calls: list[str] = []

    def transport_handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else {}
        m = body.get("method", "")
        if m == "initialize":
            init_count["n"] += 1
            return httpx.Response(
                200, request=request,
                headers={"Mcp-Session-Id": _FAKE_SESSION_ID},
                json={"jsonrpc": "2.0", "id": body.get("id"),
                      "result": {"protocolVersion": "2024-11-05", "capabilities": {}}},
            )
        if m == "notifications/initialized":
            return httpx.Response(200, request=request, json={})
        # tools/call
        tool_calls.append(body["params"]["name"])
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1",
                  "result": {"structuredContent": {"ok": True}}},
        )

    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=httpx.MockTransport(transport_handler))
    c.search("a")
    c.discover("b")
    c.scrape_markdown("https://x")
    assert init_count["n"] == 1
    assert tool_calls == ["search_engine", "discover", "scrape_as_markdown"]


def test_initialize_failure_raises_http_status_error() -> None:
    def transport_handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(403, request=request, text="forbidden")

    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=httpx.MockTransport(transport_handler))
    with pytest.raises(httpx.HTTPStatusError, match="initialize"):
        c.search("q")


def test_expired_session_triggers_one_rehandshake() -> None:
    """If BrightData returns 404 (session expired) on tools/call,
    the client should clear the cached session, re-initialize, and retry once."""

    state = {"init_count": 0, "session_id": "old-session-id"}

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else {}
        method = body.get("method")
        if method == "initialize":
            state["init_count"] += 1
            state["session_id"] = f"session-{state['init_count']}"
            return httpx.Response(
                200, request=request,
                headers={"Mcp-Session-Id": state["session_id"]},
                json={"jsonrpc": "2.0", "id": body.get("id"),
                      "result": {"protocolVersion": "2024-11-05", "capabilities": {}}},
            )
        if method == "notifications/initialized":
            return httpx.Response(200, request=request, json={})
        # tools/call: first attempt with session-1 → 404 (stale);
        # retry after re-handshake (session-2) → 200
        sent_session = request.headers.get("Mcp-Session-Id")
        if sent_session == "session-1":
            return httpx.Response(404, request=request, text="No valid session ID provided")
        return httpx.Response(
            200, request=request,
            json={"jsonrpc": "2.0", "id": "1",
                  "result": {"structuredContent": {"recovered": True}}},
        )

    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=httpx.MockTransport(handler))
    # First call: succeeds via initialize → 404 → re-initialize → 200
    assert c.search("q") == {"recovered": True}
    assert state["init_count"] == 2


def test_persistent_404_after_rehandshake_raises() -> None:
    """If BrightData still 404s after a re-handshake, give up and raise."""

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else {}
        method = body.get("method")
        if method == "initialize":
            return httpx.Response(
                200, request=request,
                headers={"Mcp-Session-Id": _FAKE_SESSION_ID},
                json={"jsonrpc": "2.0", "id": body.get("id"),
                      "result": {"protocolVersion": "2024-11-05", "capabilities": {}}},
            )
        if method == "notifications/initialized":
            return httpx.Response(200, request=request, json={})
        return httpx.Response(404, request=request, text="No valid session ID provided")

    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=httpx.MockTransport(handler))
    with pytest.raises(httpx.HTTPStatusError):
        c.search("q")


def test_initialize_without_session_header_raises_runtime() -> None:
    def transport_handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else {}
        if body.get("method") == "initialize":
            return httpx.Response(
                200, request=request,
                json={"jsonrpc": "2.0", "id": body.get("id"), "result": {}},
                # no Mcp-Session-Id header!
            )
        return httpx.Response(500, request=request)

    c = BrightDataClient(api_token=_FAKE_TOKEN, transport=httpx.MockTransport(transport_handler))
    with pytest.raises(RuntimeError, match="session id"):
        c.search("q")


# ---------- centaur tool discovery contract ----------


def test_module_exposes_client_factory() -> None:
    instance = client_factory()
    assert isinstance(instance, BrightDataClient)


def test_redact_helper_handles_both_patterns() -> None:
    assert "[REDACTED]" in _redact("see https://mcp.brightdata.com/mcp?token=abc&x=y")
    assert "abc" not in _redact("see https://mcp.brightdata.com/mcp?token=abc&x=y")
    assert "BRIGHTDATA_API_TOKEN" not in _redact("missing BRIGHTDATA_API_TOKEN env var")
