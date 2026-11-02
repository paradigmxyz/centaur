"""Tests for the Mercator CLI."""

import json
from unittest.mock import MagicMock

import httpx
from typer.testing import CliRunner

from mercator import cli


def test_search_explains_how_to_pass_attribution_with_the_cli(monkeypatch):
    client = MagicMock()
    client.__enter__.return_value = client
    client.search.return_value = {
        "id": "11111111-1111-4111-8111-111111111111",
        "endpoints": [],
        "next_action": "Call quote_plan with this id.",
    }
    monkeypatch.setattr(cli, "_client", lambda: client)

    result = CliRunner().invoke(cli.app, ["search", "current weather"])

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout) == {
        "id": "11111111-1111-4111-8111-111111111111",
        "endpoints": [],
        "next_action": (
            "Call quote_plan with this id. In this CLI, pass the returned id via --search-id "
            "to quote and submit. Keep id and searchId outside the plan JSON."
        ),
    }


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
    client.submit.assert_called_once_with(
        plan, max_spend=0.05, idempotency_key="job-0001", search_id=None
    )


def test_quote_and_submit_accept_search_id_separately_from_plan(monkeypatch):
    client = MagicMock()
    client.__enter__.return_value = client
    client.quote.return_value = {"totalAmount": "0.006", "quoteToken": "signed-quote"}
    client.submit.return_value = {"job": {"status": "pending"}}
    monkeypatch.setattr(cli, "_client", lambda: client)
    plan = {"nodes": []}
    search_id = "11111111-1111-4111-8111-111111111111"

    for command, extra in (
        ("quote", []),
        ("submit", ["--max-spend", "0.05", "--idempotency-key", "job-0001"]),
    ):
        result = CliRunner().invoke(
            cli.app, [command, "-", "--search-id", search_id, *extra], input=json.dumps(plan)
        )
        assert result.exit_code == 0, result.output

    client.quote.assert_called_once_with(plan, search_id=search_id)
    client.submit.assert_called_once_with(
        plan, max_spend=0.05, idempotency_key="job-0001", search_id=search_id
    )


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
