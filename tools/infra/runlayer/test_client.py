import json

import httpx
import pytest
from centaur_tool_runlayer.client import RunlayerClient, RunlayerError

URL = "https://tenant.runlayer.com/mcp"
TOOL = {"name": "read_widget", "inputSchema": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}}


def response(request, result, **kwargs):
    return httpx.Response(200, json={"jsonrpc": "2.0", "id": json.loads(request.content)["id"], "result": result}, **kwargs)


def test_configuration_lifecycle_pagination_and_schema(monkeypatch):
    monkeypatch.setenv("CENTAUR_CONSOLE_URL", "http://console.local")
    monkeypatch.setenv("RUNLAYER_MCP_URL", URL)
    seen = []
    def handler(request):
        seen.append(request)
        assert "Authorization" not in request.headers  # injected outside the sandbox
        if request.url.host == "console.local":
            return httpx.Response(200, json={"data": [{"provider": "runlayer", "labels": {}, "start_url": "https://console.test/oauth/runlayer/start"}]})
        if request.method == "DELETE":
            return httpx.Response(204)
        body = json.loads(request.content)
        if body["method"] == "initialize":
            return response(request, {"protocolVersion": "2025-03-26"}, headers={"Mcp-Session-Id": "person-session"})
        assert request.headers["Mcp-Session-Id"] == "person-session"
        if body["method"] == "notifications/initialized":
            return httpx.Response(202)
        if body["method"] == "tools/list":
            if not body["params"].get("cursor"):
                return response(request, {"tools": [], "nextCursor": "second"})
            return response(request, {"tools": [TOOL]})
        return response(request, {"content": [{"type": "text", "text": "ok"}]})
    with httpx.Client(transport=httpx.MockTransport(handler)) as http:
        with RunlayerClient(http_client=http) as client:
            assert client.list_tools() == [TOOL]
            assert client._call_tool("read_widget", {"id": 1}, tools=[TOOL])["content"][0]["text"] == "ok"
            with pytest.raises(RunlayerError, match="schema"):
                client._call_tool("read_widget", {"id": "bad"}, tools=[TOOL])
            with pytest.raises(RunlayerError, match="Unknown"):
                client._call_tool("invented", {}, tools=[TOOL])
    assert seen[-1].method == "DELETE"


def test_sse_stops_at_reply_and_preserves_tool_approvals():
    def handler(request):
        body = json.loads(request.content)
        data = {"jsonrpc": "2.0", "id": body["id"], "result": {"isError": True, "approval_required": True}}
        return httpx.Response(200, text=f'data: {{"method":"notification"}}\n\ndata: {json.dumps(data)}\n\n', headers={"Content-Type": "text/event-stream"})
    with httpx.Client(transport=httpx.MockTransport(handler)) as http:
        client = RunlayerClient(URL, http_client=http)
        assert client._call_tool("read_widget", {"id": 1}, tools=[TOOL])["approval_required"]


@pytest.mark.parametrize("status", [401, 403, 500])
def test_tool_calls_are_never_replayed(status):
    calls = []
    def handler(request):
        calls.append(request)
        return httpx.Response(status, headers={"X-Runlayer-Connect-URL": "https://tenant.runlayer.com/connect?server=one"})
    with httpx.Client(transport=httpx.MockTransport(handler)) as http:
        client = RunlayerClient(URL, http_client=http)
        with pytest.raises(RunlayerError) as failure:
            client._call_tool("read_widget", {"id": 1}, tools=[TOOL])
        assert failure.value.connect_url == "https://tenant.runlayer.com/connect?server=one"
    assert len(calls) == 1


def test_new_person_has_no_previous_session_or_tool_cache():
    with httpx.Client(transport=httpx.MockTransport(lambda _: httpx.Response(401))) as http:
        alice = RunlayerClient(URL, http_client=http)
        alice._session = "alice"
        bob = RunlayerClient(URL, http_client=http)
        assert bob._session is None
        assert not bob._initialized


@pytest.mark.parametrize("url", ["http://tenant/mcp", "https://u:p@tenant/mcp", "https://tenant/mcp?token=x", "https://tenant", "https://tenant:invalid/mcp", {"bad": "url"}])
def test_rejects_unsafe_resource_urls(url):
    with pytest.raises(RunlayerError):
        RunlayerClient(url)


def test_schema_cannot_fetch_remote_references():
    tool = {"name": "remote_schema", "inputSchema": {"$ref": "https://unrelated.test/schema"}}
    with httpx.Client(transport=httpx.MockTransport(lambda _: pytest.fail("no network request expected"))) as http:
        client = RunlayerClient(URL, http_client=http)
        with pytest.raises(RunlayerError, match="schema"):
            client._call_tool("remote_schema", {}, tools=[tool])


def test_unsupported_negotiated_protocol_stops_before_discovery():
    calls = []
    def handler(request):
        calls.append(request)
        return response(request, {"protocolVersion": "unknown"})
    with httpx.Client(transport=httpx.MockTransport(handler)) as http:
        with RunlayerClient(URL, http_client=http) as client:
            with pytest.raises(RunlayerError, match="unsupported MCP protocol"):
                client.list_tools()
    assert len(calls) == 1
