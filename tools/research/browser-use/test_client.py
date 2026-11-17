"""Tests for the Browser Use client."""

import json

import httpx
import pytest
from centaur_tool_browser_use.client import BrowserUseClient, BrowserUseError, output_text


def client_with(handler) -> BrowserUseClient:
    client = BrowserUseClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.browser-use.com", transport=httpx.MockTransport(handler)
    )
    return client


def test_browsers_use_the_v4_api_and_never_return_the_cdp_url():
    seen = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append((request.method, request.url.path, json.loads(request.content or b"null")))
        assert request.headers["x-browser-use-api-key"] == "test-key"
        return httpx.Response(
            200,
            json={
                "id": "b1",
                "status": "active",
                "cdpUrl": "https://b1.cdp.example",
                "liveUrl": "https://live",
            },
        )

    with client_with(handler) as client:
        created = client.create_browser(timeout_minutes=5, proxy_country_code="de")
        stopped = client.stop_browser("b1")

    assert "cdpUrl" not in created and "cdpUrl" not in stopped
    assert created["liveUrl"] == "https://live"
    assert seen == [
        ("POST", "/api/v4/browsers", {"timeout": 5, "proxyCountryCode": "de"}),
        ("PATCH", "/api/v4/browsers/b1", {"action": "stop"}),
    ]


def test_respond_sends_a_stateless_responses_request():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/v4/llm/v1/responses"
        assert json.loads(request.content) == {
            "model": "bu-4.0",
            "input": "hi",
            "store": False,
            "stream": False,
            "reasoning": {"effort": "low"},
        }
        return httpx.Response(
            200,
            json={
                "id": "r1",
                "status": "completed",
                "output": [
                    {"type": "reasoning", "summary": []},
                    {"type": "message", "content": [{"type": "output_text", "text": "hello"}]},
                ],
            },
        )

    with client_with(handler) as client:
        response = client.respond("hi", reasoning_effort="low")

    assert output_text(response) == "hello"


def test_errors_carry_the_api_message_not_the_key():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            401, json={"error": {"code": "invalid_api_key", "message": "Invalid API key."}}
        )

    with client_with(handler) as client, pytest.raises(BrowserUseError) as raised:
        client.search("x")

    assert "HTTP 401: Invalid API key." in str(raised.value)
    assert "test-key" not in str(raised.value)
