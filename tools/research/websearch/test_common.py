from __future__ import annotations

import tomllib
from pathlib import Path

import httpx
from centaur_tool_websearch import _common


def test_tool_version_matches_pyproject() -> None:
    manifest = tomllib.loads(Path(__file__).with_name("pyproject.toml").read_text())
    assert manifest["project"]["version"] == _common.FALLBACK_VERSION


def test_append_within_budget_protects_trailer() -> None:
    trailer = "\n\n## Sources\n[1] t — u"
    out = _common.append_within_budget("x" * 100, trailer, 40)
    assert out == "x" * (40 - len(trailer)) + trailer
    assert len(out) == 40
    assert _common.append_within_budget("short", trailer, 400) == "short" + trailer


def test_append_within_budget_keeps_an_oversized_trailer_whole() -> None:
    trailer = "\n\n## Sources\n[1] t \u2014 u"
    out = _common.append_within_budget("x" * 100, trailer, 5)
    assert out == trailer
    assert len(out) > 5


def test_decode_jsonrpc_response_handles_json_and_sse() -> None:
    plain = httpx.Response(200, json={"jsonrpc": "2.0", "result": {"a": 1}})
    assert _common.decode_jsonrpc_response(plain) == {"jsonrpc": "2.0", "result": {"a": 1}}
    sse = httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        content=b'event: message\ndata: {"jsonrpc": "2.0", "result": {"b": 2}}\n\n',
    )
    assert _common.decode_jsonrpc_response(sse) == {"jsonrpc": "2.0", "result": {"b": 2}}


def test_decode_jsonrpc_response_matches_the_request_id_not_the_last_frame() -> None:
    sse = httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        content=(
            b'event: message\ndata: {"jsonrpc": "2.0", "id": "req-1", "result": {"b": 2}}\n\n'
            b'event: message\ndata: {"jsonrpc": "2.0", "method": "notifications/progress"}\n\n'
        ),
    )

    assert _common.decode_jsonrpc_response(sse, "req-1") == {
        "jsonrpc": "2.0",
        "id": "req-1",
        "result": {"b": 2},
    }


def test_decode_jsonrpc_response_falls_back_when_no_frame_carries_the_id() -> None:
    sse = httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        content=b'event: message\ndata: {"jsonrpc": "2.0", "result": {"b": 2}}\n\n',
    )

    assert _common.decode_jsonrpc_response(sse, "req-1") == {"jsonrpc": "2.0", "result": {"b": 2}}
