"""Verify raw response preservation and SDK requests."""

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
from .client import API_KEY_SECRET, ArtemisClient


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
        assert request.headers["X-API-Key"] == API_KEY_SECRET
        assert "APIKey" not in request.url.params
        return response({})

    with patch(f"{ArtemisClient.__module__}.secret", return_value=API_KEY_SECRET) as secret:
        with mock_client(handler, api_key=api_key) as client:
            client.get_market_data(["BTC"])
        secret.assert_called_once_with(API_KEY_SECRET, "")


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
