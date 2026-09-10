import json

import httpx
import pytest
from client import PayboxClient


def rpc_response(payload: dict, session_id: str | None = None) -> httpx.Response:
    headers = {"content-type": "application/json"}
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    return httpx.Response(200, headers=headers, json=payload)


def test_initializes_session_and_lists_tools() -> None:
    requests: list[dict] = []

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        requests.append(body)
        if body["method"] == "initialize":
            assert request.headers["authorization"] == "Bearer token"
            return rpc_response({"jsonrpc": "2.0", "id": body["id"], "result": {}}, "session-1")
        if body["method"] == "notifications/initialized":
            assert request.headers["mcp-session-id"] == "session-1"
            return httpx.Response(202)
        assert request.headers["mcp-session-id"] == "session-1"
        return rpc_response(
            {
                "jsonrpc": "2.0",
                "id": body["id"],
                "result": {"tools": [{"name": "list_credentials"}]},
            }
        )

    client = PayboxClient(
        token="token", http_client=httpx.Client(transport=httpx.MockTransport(handler))
    )
    try:
        assert client.list_tools() == [{"name": "list_credentials"}]
    finally:
        client.close()
    assert [request["method"] for request in requests] == [
        "initialize",
        "notifications/initialized",
        "tools/list",
    ]


def test_execute_requires_confirmation_for_sensitive_tools() -> None:
    client = PayboxClient(
        token="token", http_client=httpx.Client(transport=httpx.MockTransport(lambda _: None))
    )
    try:
        with pytest.raises(RuntimeError, match="explicit user approval"):
            client.execute("request_swap", {"amount": "1"})
    finally:
        client.close()


def test_tool_call_prefers_structured_content() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        if body["method"] == "initialize":
            return rpc_response({"jsonrpc": "2.0", "id": body["id"], "result": {}})
        if body["method"] == "notifications/initialized":
            return httpx.Response(202)
        return rpc_response(
            {
                "jsonrpc": "2.0",
                "id": body["id"],
                "result": {
                    "content": [{"type": "text", "text": "fallback"}],
                    "structuredContent": {"credentials": []},
                },
            }
        )

    client = PayboxClient(
        token="token", http_client=httpx.Client(transport=httpx.MockTransport(handler))
    )
    try:
        assert client.list_credentials() == {"credentials": []}
    finally:
        client.close()
