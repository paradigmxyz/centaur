"""Shared mocked transport and sample values for Artemis tests."""

from functools import partial
from unittest.mock import patch

import httpx

from .client import ArtemisClient

TEST_KEY = "test-artemis-key"
TEST_PRICE = 25.0
TEST_FRACTION = -0.025
HTTP_OK = 200
HTTP_UNAUTHORIZED = 401
EXIT_ERROR = 1
UPSTREAM_ERROR = "Metric not available for asset."


def mock_client(handler, api_key=TEST_KEY):
    client = ArtemisClient(api_key=api_key)
    factory = partial(httpx.Client, transport=httpx.MockTransport(handler))
    with patch.object(httpx, "Client", side_effect=factory):
        _ = client.client
    return client


def response(symbols):
    return httpx.Response(HTTP_OK, json={"data": {"symbols": symbols}})
