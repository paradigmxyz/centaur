"""Tests for the Pangram CLI."""

import json
from unittest.mock import MagicMock

import httpx
from typer.testing import CliRunner

from pangram import cli


def test_detect_outputs_the_completed_result(monkeypatch):
    client = MagicMock()
    client.__enter__.return_value = client
    client.detect.return_value = {"stage": "STAGE_SUCCESS", "prediction_short": "Human"}
    monkeypatch.setattr(cli, "_client", lambda: client)

    result = CliRunner().invoke(
        cli.app,
        ["detect", "A passage", "--model", "pangram-4", "--public-dashboard-link"],
    )

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["prediction_short"] == "Human"
    client.detect.assert_called_once_with(
        "A passage",
        model="pangram-4",
        public_dashboard_link=True,
        task_timeout=300.0,
        poll_interval=1.0,
    )


def test_http_error_does_not_print_response_body(monkeypatch):
    request = httpx.Request("GET", "https://text.external-api.pangram.com/models")
    response = httpx.Response(401, request=request, text="secret response")
    client = MagicMock()
    client.__enter__.return_value = client
    client.list_models.side_effect = httpx.HTTPStatusError(
        "unauthorized", request=request, response=response
    )
    monkeypatch.setattr(cli, "_client", lambda: client)

    result = CliRunner().invoke(cli.app, ["models"])

    assert result.exit_code == 1
    assert "HTTP 401" in result.output
    assert "secret response" not in result.output
