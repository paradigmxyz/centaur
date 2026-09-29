"""Tests for the Mercator CLI."""

import json
from unittest.mock import MagicMock

import httpx
from typer.testing import CliRunner

from mercator import cli


def test_submit_passes_the_plan_spend_limit_and_idempotency_key(monkeypatch, tmp_path):
    plan = {
        "nodes": [{"id": "weather", "serviceId": "openweather", "method": "POST", "path": "/w"}]
    }
    plan_path = tmp_path / "plan.json"
    plan_path.write_text(json.dumps(plan))
    client = MagicMock()
    client.__enter__.return_value = client
    client.submit.return_value = {"job": {"jobId": "job-123", "status": "pending"}}
    monkeypatch.setattr(cli, "_client", lambda: client)

    result = CliRunner().invoke(
        cli.app,
        ["submit", str(plan_path), "--max-spend", "0.05", "--idempotency-key", "job-0001"],
    )

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["job"]["jobId"] == "job-123"
    client.submit.assert_called_once_with(plan, max_spend=0.05, idempotency_key="job-0001")


def test_http_error_does_not_print_response_body(monkeypatch):
    request = httpx.Request("POST", "https://mercator.sh/mcp/auth")
    response = httpx.Response(401, request=request, text="secret response")
    client = MagicMock()
    client.__enter__.return_value = client
    client.search.side_effect = httpx.HTTPStatusError(
        "unauthorized", request=request, response=response
    )
    monkeypatch.setattr(cli, "_client", lambda: client)

    result = CliRunner().invoke(cli.app, ["search", "current weather"])

    assert result.exit_code == 1
    assert "HTTP 401" in result.output
    assert "secret response" not in result.output
