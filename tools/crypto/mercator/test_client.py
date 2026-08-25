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


def test_submit_job_uses_centaur_policy_and_returns_result_with_receipt():
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
        if request.url == httpx.URL("http://api:8080/api/mercator/jobs"):
            assert request.headers["authorization"] == "Bearer sandbox-principal"
            assert json.loads(request.content) == {"approved": False, "handoff": handoff}
            return httpx.Response(
                200,
                json={
                    "ok": True,
                    "approval": "automatic",
                    "result": {"jobId": "job-1", "ready": False},
                },
            )
        body = json.loads(request.content)
        assert body["params"] == {
            "name": "get_job",
            "arguments": {"job_id": "job-1"},
        }
        return httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "structuredContent": {
                        "job": {
                            "cached": True,
                            "jobId": "job-1",
                            "ok": True,
                            "ready": True,
                            "result": {"price": {"usd": 65000}},
                            "usage": {
                                "committed": "0.001",
                                "currency": "USDC.e",
                                "paymentMethod": "charge",
                            },
                        }
                    }
                },
            },
        )

    with MercatorClient(
        centaur_api_url="http://api:8080",
        bearer_token="sandbox-principal",
        transport=httpx.MockTransport(handler),
    ) as client:
        result = client.submit_job(handoff, poll_interval=0)
    assert result["approval"] == "automatic"
    assert result["job"]["result"] == {"price": {"usd": 65000}}
    assert result["receipt"] == {
        "jobId": "job-1",
        "usage": {
            "committed": "0.001",
            "currency": "USDC.e",
            "paymentMethod": "charge",
        },
    }


def test_submit_job_can_return_without_polling():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, json={"ok": True, "result": {"jobId": "job-1", "ready": False}}
        )

    with MercatorClient(
        centaur_api_url="http://api:8080",
        transport=httpx.MockTransport(handler),
    ) as client:
        result = client.submit_job({}, wait=False)
    assert result["result"] == {"jobId": "job-1", "ready": False}


def test_submit_job_timeout_keeps_recoverable_job_id():
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url == httpx.URL("http://api:8080/api/mercator/jobs"):
            return httpx.Response(
                200, json={"ok": True, "result": {"jobId": "job-1", "ready": False}}
            )
        return httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "structuredContent": {"job": {"jobId": "job-1", "ready": False}}
                },
            },
        )

    with MercatorClient(
        centaur_api_url="http://api:8080",
        transport=httpx.MockTransport(handler),
    ) as client:
        result = client.submit_job({}, poll_interval=0, wait_timeout=0)
    assert result["job"] == {"jobId": "job-1", "ready": False}
    assert result["polling"]["status"] == "timed_out"


def test_full_search_quote_create_submit_poll_workflow():
    plan = {
        "nodes": [
            {
                "id": "btc-price",
                "serviceId": "x402-api",
                "method": "GET",
                "path": "/crypto/price/btc/usd/btc-usd",
                "input": {},
                "dependsOn": [],
            }
        ]
    }
    handoff = {
        "status": "payment_required",
        "nextAction": "run_rest_request",
        "maxSpend": "0.004",
        "rest": {
            "method": "POST",
            "url": "https://mercator.tempoxyz.dev/v1/jobs",
            "body": {"idempotencyKey": "centaur-e2e-1", "plan": plan},
        },
    }

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        if request.url == httpx.URL("http://api:8080/api/mercator/jobs"):
            assert body == {"approved": False, "handoff": handoff}
            return httpx.Response(
                200,
                json={
                    "ok": True,
                    "approval": "automatic",
                    "result": {"jobId": "job-e2e", "ready": False},
                },
            )
        name = body["params"]["name"]
        structured = {
            "search_services": {"matches": [{"serviceId": "x402-api"}]},
            "quote_plan": {"plan": plan, "totalAmount": "0.004"},
            "create_job": {"handoff": handoff},
            "get_job": {
                "job": {
                    "cached": True,
                    "jobId": "job-e2e",
                    "ok": True,
                    "ready": True,
                    "result": {"btc-price": {"usd": 65000}},
                    "usage": {
                        "committed": "0.004",
                        "currency": "USDC.e",
                        "paymentMethod": "charge",
                    },
                }
            },
        }[name]
        return httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"structuredContent": structured},
            },
        )

    with MercatorClient(
        mcp_url="https://mercator.example/mcp",
        centaur_api_url="http://api:8080",
        transport=httpx.MockTransport(handler),
    ) as client:
        assert client.search_services("current BTC/USD price")["matches"]
        quote = client.quote_plan(plan)
        created = client.create_job(quote["plan"], "centaur-e2e-1")
        completed = client.submit_job(created["handoff"], poll_interval=0)

    assert completed["job"]["result"] == {"btc-price": {"usd": 65000}}
    assert completed["receipt"]["usage"]["committed"] == "0.004"


def test_payer_error_does_not_expose_non_json_response_body():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(503, text="wallet-private-material")

    with MercatorClient(
        centaur_api_url="http://api:8080",
        transport=httpx.MockTransport(handler),
    ) as client:
        with pytest.raises(RuntimeError, match="returned HTTP 503") as raised:
            client.submit_job({}, approved=True, wait=False)
    assert "wallet-private-material" not in str(raised.value)
