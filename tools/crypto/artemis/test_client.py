"""Verify raw response preservation and HTTP requests."""

from http import HTTPStatus
from unittest.mock import patch

import httpx
import pytest

from ._test_helpers import (
    HTTP_OK,
    TEST_FRACTION,
    TEST_KEY,
    TEST_PRICE,
    UPSTREAM_ERROR,
    mock_client,
    response,
)
from .client import ArtemisClient


def test_raw_response_and_request_symbols_are_preserved():
    payload = {
        "data": {
            "symbols": {
                "btc": {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": TEST_FRACTION},
                "eth": {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": 0},
            }
        },
        "metadata": {"source": "Artemis"},
    }

    def handler(request):
        assert request.url.path == "/data/api/PRICE,24H_PRICE_CHG_PCT/"
        assert dict(request.url.params) == {"symbols": "BTC,eth,btc"}
        assert request.headers["X-API-Key"] == TEST_KEY
        assert TEST_KEY not in str(request.url)
        return httpx.Response(HTTP_OK, json=payload)

    with mock_client(handler) as client:
        result = client.get_market_data(["BTC", "eth", "btc"])

    assert result == payload
    assert client._client is None


def test_partial_results_preserve_upstream_errors_nulls_and_missing_fields():
    assets = {
        "btc": {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": UPSTREAM_ERROR},
        "eth": {"PRICE": None},
    }
    with mock_client(lambda _: response(assets)) as client:
        result = client.get_market_data(["BTC", "ETH", "MISSING"])

    assert result == {"data": {"symbols": assets}}


@pytest.mark.parametrize("api_key", [None, ""])
def test_sdk_secret_placeholder_goes_in_header(api_key):
    def handler(request):
        assert request.headers["X-API-Key"] == "ARTEMIS_API_KEY"
        assert "APIKey" not in request.url.params
        return response({})

    with patch(f"{ArtemisClient.__module__}.secret", return_value="ARTEMIS_API_KEY") as secret:
        with mock_client(handler, api_key=api_key) as client:
            client.get_market_data(["BTC"])
        secret.assert_called_once_with("ARTEMIS_API_KEY", "")


def test_missing_api_key_omits_auth_header():
    def handler(request):
        assert "APIKey" not in request.url.params
        assert "X-API-Key" not in request.headers
        return response({})

    with (
        patch(f"{ArtemisClient.__module__}.secret", return_value=""),
        mock_client(handler, api_key="") as client,
    ):
        client.get_market_data(["BTC"])


@pytest.mark.parametrize(
    "status",
    [HTTPStatus.UNAUTHORIZED, HTTPStatus.TOO_MANY_REQUESTS, HTTPStatus.SERVICE_UNAVAILABLE],
)
def test_http_errors_propagate_without_retries(status):
    attempts = []

    def handler(request):
        attempts.append(request)
        return httpx.Response(status)

    with mock_client(handler) as client, pytest.raises(httpx.HTTPStatusError):
        client.get_market_data(["BTC"])
    assert len(attempts) == 1


def test_default_client_timeout_and_redirect_policy():
    with ArtemisClient() as client:
        assert client.client.timeout == httpx.Timeout(30.0)
        assert client.client.follow_redirects is False
