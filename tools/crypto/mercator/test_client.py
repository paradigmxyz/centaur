import json

import httpx
import pytest

from client import MercatorClient


def test_search_services_uses_remote_mcp_contract():
    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        assert request.url == httpx.URL("https://mercator.example/mcp")
        assert body["method"] == "tools/call"
        assert body["params"] == {
            "name": "search_services",
            "arguments": {
                "query": "find current company information",
                "limit": 5,
                "resolution": "static",
            },
        }
        return httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"structuredContent": {"matches": []}},
            },
        )

    with MercatorClient(
        mcp_url="https://mercator.example/mcp",
        transport=httpx.MockTransport(handler),
    ) as client:
        assert client.search_services("find current company information", limit=5) == {
            "matches": []
        }


def test_submit_job_requires_explicit_approval_and_uses_centaur_payer():
    handoff = {
        "status": "payment_required",
        "nextAction": "run_rest_request",
        "maxSpend": "0.001",
        "rest": {
            "method": "POST",
            "url": "https://mercator.tempoxyz.dev/v1/jobs",
            "body": {"idempotencyKey": "centaur-test", "plan": {"nodes": []}},
        },
    }

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url == httpx.URL("http://api:8080/api/mercator/jobs")
        assert request.headers["authorization"] == "Bearer sandbox-principal"
        assert json.loads(request.content) == {"approved": True, "handoff": handoff}
        return httpx.Response(200, json={"ok": True, "result": {"job": {"id": "1"}}})

    with MercatorClient(
        centaur_api_url="http://api:8080",
        bearer_token="sandbox-principal",
        transport=httpx.MockTransport(handler),
    ) as client:
        with pytest.raises(ValueError, match="explicit user approval"):
            client.submit_job(handoff)
        assert client.submit_job(handoff, approved=True)["result"]["job"]["id"] == "1"


def test_payer_error_does_not_expose_non_json_response_body():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(503, text="wallet-private-material")

    with MercatorClient(
        centaur_api_url="http://api:8080",
        transport=httpx.MockTransport(handler),
    ) as client:
        with pytest.raises(RuntimeError, match="returned HTTP 503") as raised:
            client.submit_job({}, approved=True)
    assert "wallet-private-material" not in str(raised.value)
