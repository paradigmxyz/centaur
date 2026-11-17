"""Tests for the Browser Use CLI."""

import json
from unittest.mock import MagicMock

from centaur_tool_browser_use import cli
from centaur_tool_browser_use.client import BrowserUseError
from typer.testing import CliRunner


def fake_client(monkeypatch) -> MagicMock:
    client = MagicMock()
    client.__enter__.return_value = client
    monkeypatch.setattr(cli, "_client", lambda: client)
    return client


def test_respond_prints_the_output_text(monkeypatch):
    client = fake_client(monkeypatch)
    client.respond.return_value = {
        "output": [{"type": "message", "content": [{"type": "output_text", "text": "hi"}]}],
        "usage": {"input_tokens": 1},
    }

    result = CliRunner().invoke(cli.app, ["respond", "hello", "--model", "bu-4.0-mini"])

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["output_text"] == "hi"
    client.respond.assert_called_once_with(
        "hello",
        model="bu-4.0-mini",
        instructions=None,
        max_output_tokens=None,
        reasoning_effort=None,
    )


def test_health_reports_failures(monkeypatch):
    client = fake_client(monkeypatch)
    client.list_models.side_effect = BrowserUseError(
        "Browser Use returned HTTP 401: Invalid API key."
    )

    result = CliRunner().invoke(cli.app, ["health"])

    assert result.exit_code == 1
    assert json.loads(result.stdout)["ok"] is False


def test_browser_create_passes_options(monkeypatch):
    client = fake_client(monkeypatch)
    client.create_browser.return_value = {"id": "b1", "status": "active"}

    result = CliRunner().invoke(cli.app, ["browser", "create", "--timeout-minutes", "5"])

    assert result.exit_code == 0, result.output
    client.create_browser.assert_called_once_with(
        timeout_minutes=5, profile_id=None, proxy_country_code=None, network_policy_id=None
    )
