"""Shared mocked transport and sample values for Artemis tests."""

import httpx

from artemis import Artemis

from .client import BASE_URL, ArtemisClient

TEST_KEY = "test-artemis-key"
TEST_PRICE = 25.0
TEST_FRACTION = -0.025
HTTP_OK = 200
HTTP_UNAUTHORIZED = 401
EXIT_ERROR = 1
UPSTREAM_ERROR = "Metric not available for asset."


def mock_client(handler, api_key=TEST_KEY):
    client = ArtemisClient(api_key=api_key)
    client._client = Artemis(
        api_key="",
        base_url=BASE_URL,
        max_retries=0,
        http_client=httpx.Client(transport=httpx.MockTransport(handler)),
    )
    return client


def response(symbols):
    return httpx.Response(HTTP_OK, json={"data": {"symbols": symbols}})
