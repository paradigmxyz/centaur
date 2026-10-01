"""Tests for the Mercator API client."""

import json

import httpx
import pytest

from mercator.client import MercatorClient

PLAN = {"nodes": [{"id": "weather", "serviceId": "openweather", "method": "POST", "path": "/w"}]}


def _mercator(handler):
    calls = []

    def record(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        calls.append((request, body["params"]["name"], body["params"]["arguments"]))
        structured = handler(body["params"]["name"], body["params"]["arguments"])
        return httpx.Response(200, json={"result": {"structuredContent": structured}})

    client = MercatorClient()
    client._client = httpx.Client(transport=httpx.MockTransport(record))
    return client, calls


def test_search_calls_the_hosted_mcp_tool_without_sending_credentials():
    client, calls = _mercator(lambda tool, arguments: {"endpoints": []})

    assert client.search("current weather", limit=3) == {"endpoints": []}

    request, tool, arguments = calls[0]
    assert request.url == "https://mercator.sh/mcp/auth"
    assert json.loads(request.content)["method"] == "tools/call"
    assert (tool, arguments) == ("search_services", {"query": "current weather", "limit": 3})
    assert "authorization" not in request.headers


def test_submit_refuses_a_quote_over_max_spend():
    client, calls = _mercator(lambda tool, arguments: {"totalAmount": "0.25"})

    with pytest.raises(ValueError, match="nothing was paid"):
        client.submit(PLAN, max_spend=0.1, idempotency_key="job-0001")

    assert [tool for _, tool, _ in calls] == ["quote_plan"]


def test_submit_approves_the_exact_quoted_total():
    def handler(tool, arguments):
        return {"totalAmount": "0.006"} if tool == "quote_plan" else {"job": {"status": "pending"}}

    client, calls = _mercator(handler)

    result = client.submit(PLAN, max_spend=0.01, idempotency_key="job-0001")

    assert result == {"job": {"status": "pending"}}
    assert calls[-1][1:] == (
        "create_job",
        {"plan": PLAN, "approved_total": "0.006", "idempotency_key": "job-0001"},
    )


def test_call_reads_a_response_sent_as_an_event_stream():
    progress = {"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}
    response = {"jsonrpc": "2.0", "id": 1, "result": {"structuredContent": {"endpoints": []}}}
    stream = "".join(f"event: message\ndata: {json.dumps(m)}\n\n" for m in (progress, response))
    client = MercatorClient()
    client._client = httpx.Client(
        transport=httpx.MockTransport(
            lambda request: httpx.Response(
                200, headers={"content-type": "text/event-stream"}, text=stream
            )
        )
    )

    assert client.search("current weather") == {"endpoints": []}
