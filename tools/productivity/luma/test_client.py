from unittest.mock import patch

import httpx
import pytest
from client import LumaClient


def _mock_client(handler) -> LumaClient:
    client = LumaClient(api_key="test-key")
    client._http_client = httpx.Client(
        base_url="https://public-api.luma.com",
        headers={"x-luma-api-key": "test-key"},
        transport=httpx.MockTransport(handler),
    )
    return client


def test_list_events_sends_auth_filters_and_organization_calendar() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == "/v1/calendars/events/list"
        assert request.headers["x-luma-api-key"] == "test-key"
        assert request.headers["x-luma-calendar-id"] == "cal-123"
        assert request.url.params.get_list("platforms") == ["luma", "external"]
        assert request.url.params.get_list("access") == ["manage", "view"]
        assert request.url.params["pagination_limit"] == "25"
        return httpx.Response(
            200,
            json={"entries": [{"id": "evt-1"}], "has_more": False},
        )

    client = _mock_client(handler)

    result = client.list_events(
        platforms=["luma", "external"],
        access=["manage", "view"],
        limit=25,
        calendar_id="cal-123",
    )

    assert result == {"entries": [{"id": "evt-1"}], "has_more": False}


def test_rate_limit_response_honors_retry_after() -> None:
    attempts = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            return httpx.Response(429, headers={"Retry-After": "0"}, json={"error": "wait"})
        return httpx.Response(200, json={"id": "usr-1"})

    client = _mock_client(handler)

    with patch("client.time.sleep") as sleep:
        assert client.get_self() == {"id": "usr-1"}

    assert attempts == 2
    sleep.assert_called_once_with(0.0)


def test_api_errors_include_nested_luma_message() -> None:
    client = _mock_client(
        lambda request: httpx.Response(
            400,
            json={"error": {"message": "event_id is required"}},
        )
    )

    with pytest.raises(RuntimeError, match=r"Luma API error \(400\): event_id is required"):
        client.get_event("")


def test_request_rejects_external_urls_before_sending() -> None:
    client = _mock_client(lambda request: pytest.fail("request should not be sent"))

    with pytest.raises(ValueError, match="not a URL"):
        client.request("GET", "https://example.com/v1/users/get-self")


def test_create_event_posts_json() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "POST"
        assert request.url.path == "/v1/events/create"
        assert request.read() == b'{"name":"Launch","start_at":"2026-09-01T17:00:00.000Z"}'
        return httpx.Response(200, json={"id": "evt-123"})

    client = _mock_client(handler)

    assert client.create_event({"name": "Launch", "start_at": "2026-09-01T17:00:00.000Z"}) == {
        "id": "evt-123"
    }
