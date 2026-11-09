"""Tests for the FRED API client."""

import httpx
import pytest
from centaur_tool_fred.client import FredClient


def _client_with(handler) -> FredClient:
    client = FredClient(api_key="test-key")
    client._client = httpx.Client(transport=httpx.MockTransport(handler))
    return client


def test_observations_sends_key_json_format_and_only_set_params():
    seen = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        return httpx.Response(200, json={"observations": [{"date": "2024-01-01", "value": "3.7"}]})

    with _client_with(handler) as client:
        result = client.get_observations(
            "UNRATE", start="2024-01-01", units="pc1", frequency="q", aggregation_method="avg"
        )

    assert result["observations"][0]["value"] == "3.7"
    (request,) = seen
    assert request.method == "GET"
    assert request.url.path == "/fred/series/observations"
    assert request.url.host == "api.stlouisfed.org"
    assert dict(request.url.params) == {
        "series_id": "UNRATE",
        "observation_start": "2024-01-01",
        "units": "pc1",
        "frequency": "q",
        "aggregation_method": "avg",
        "sort_order": "asc",
        "api_key": "test-key",
        "file_type": "json",
    }


def test_release_dates_routes_to_all_releases_without_id():
    paths = []

    def handler(request: httpx.Request) -> httpx.Response:
        paths.append((request.url.path, dict(request.url.params)))
        return httpx.Response(200, json={"release_dates": []})

    with _client_with(handler) as client:
        client.get_release_dates(limit=5)
        client.get_release_dates(10, include_future=True, sort_order="desc")

    assert paths[0][0] == "/fred/releases/dates"
    assert "release_id" not in paths[0][1]
    assert "include_release_dates_with_no_data" not in paths[0][1]
    assert paths[1][0] == "/fred/release/dates"
    assert paths[1][1]["release_id"] == "10"
    assert paths[1][1]["include_release_dates_with_no_data"] == "true"
    assert paths[1][1]["sort_order"] == "desc"


def test_http_errors_raise():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"error_code": 400, "error_message": "Bad Request."})

    with _client_with(handler) as client, pytest.raises(httpx.HTTPStatusError):
        client.get_series("NOPE")


def test_non_object_response_is_rejected():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=["unexpected"])

    with _client_with(handler) as client, pytest.raises(RuntimeError):
        client.get_series("GDP")
