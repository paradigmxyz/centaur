"""Verify CLI output, health checks, and error presentation."""

import json
from unittest.mock import patch

import httpx
import pytest
from typer.testing import CliRunner

from . import cli
from ._test_helpers import (
    EXIT_ERROR,
    HTTP_UNAUTHORIZED,
    TEST_FRACTION,
    TEST_KEY,
    TEST_PRICE,
    UPSTREAM_ERROR,
    mock_client,
    response,
)


@pytest.mark.parametrize("output", ["--json", "--markdown", None])
def test_cli_output(output):
    assets = {"btc": {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": UPSTREAM_ERROR}}
    client = mock_client(lambda _: response(assets))
    with patch.object(cli, "_client", return_value=client):
        result = CliRunner().invoke(cli.app, ["market-data", "BTC", *([output] if output else [])])
    assert result.exit_code == 0, result.output
    assert "btc" in result.output
    if output == "--json":
        assert json.loads(result.output) == {"data": {"symbols": assets}}
    else:
        assert "Metric not available" in result.output
    assert client._client is None


@pytest.mark.parametrize(
    "assets",
    [
        {},
        {"btc": {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": UPSTREAM_ERROR}},
        {"btc": {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": TEST_FRACTION}},
    ],
)
def test_health_fails_and_preserves_partial_response(assets):
    with patch.object(cli, "_client", return_value=mock_client(lambda _: response(assets))):
        result = CliRunner().invoke(cli.app, ["health"])
    assert result.exit_code == EXIT_ERROR
    payload = json.loads(result.output)
    assert payload["ok"] is False
    assert payload["details"] == {"data": {"symbols": assets}}


def test_health_succeeds_with_both_metrics():
    assets = {
        symbol: {"PRICE": TEST_PRICE, "24H_PRICE_CHG_PCT": 0} for symbol in cli.HEALTH_SYMBOLS
    }
    with patch.object(cli, "_client", return_value=mock_client(lambda _: response(assets))):
        result = CliRunner().invoke(cli.app, ["health"])
    assert result.exit_code == 0
    assert json.loads(result.output)["ok"] is True


@pytest.mark.parametrize("command", [["market-data", "BTC"], ["health"]])
def test_cli_handles_http_errors_without_exposing_response_body(command):
    with patch.object(
        cli,
        "_client",
        return_value=mock_client(lambda _: httpx.Response(HTTP_UNAUTHORIZED, text=TEST_KEY)),
    ):
        result = CliRunner().invoke(cli.app, command)

    assert result.exit_code == EXIT_ERROR
    assert str(HTTP_UNAUTHORIZED) in result.output
    assert TEST_KEY not in result.output
    assert "Traceback" not in result.output
