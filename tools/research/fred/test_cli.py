"""Tests for the FRED CLI."""

import json
from unittest.mock import MagicMock

import httpx
from centaur_tool_fred import cli
from typer.testing import CliRunner


def _mock_client(monkeypatch) -> MagicMock:
    client = MagicMock()
    client.__enter__.return_value = client
    monkeypatch.setattr(cli, "_client", lambda: client)
    return client


def _status_error(status: int, body: dict) -> httpx.HTTPStatusError:
    request = httpx.Request("GET", "https://api.stlouisfed.org/fred/series")
    response = httpx.Response(status, request=request, json=body)
    return httpx.HTTPStatusError("error", request=request, response=response)


def test_observations_latest_maps_to_desc_sort(monkeypatch):
    client = _mock_client(monkeypatch)
    client.get_observations.return_value = {"observations": [{"value": "3.9"}]}

    result = CliRunner().invoke(cli.app, ["observations", "UNRATE", "--latest", "-n", "1"])

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["observations"][0]["value"] == "3.9"
    client.get_observations.assert_called_once_with(
        "UNRATE",
        start=None,
        end=None,
        units=None,
        frequency=None,
        aggregation_method=None,
        limit=1,
        sort_order="desc",
    )


def test_error_surfaces_fred_message_with_keys_redacted(monkeypatch):
    client = _mock_client(monkeypatch)
    client.get_series.side_effect = _status_error(
        400,
        {
            "error_code": 400,
            "error_message": "Bad Request. Key abcdefghijklmnopqrstuvwxyz012345 is invalid.",
            "other": "do-not-print",
        },
    )

    result = CliRunner().invoke(cli.app, ["series", "NOPE"])

    assert result.exit_code == 1
    assert "HTTP 400: Bad Request. Key [redacted] is invalid." in result.output
    assert "abcdefghijklmnopqrstuvwxyz012345" not in result.output
    assert "do-not-print" not in result.output


def test_health_reports_failure_as_json(monkeypatch):
    client = _mock_client(monkeypatch)
    client.get_series.side_effect = _status_error(403, {"error_message": "Forbidden"})

    result = CliRunner().invoke(cli.app, ["health"])

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert payload["tool"] == "fred"
    assert "HTTP 403" in payload["error"]


def test_health_success(monkeypatch):
    client = _mock_client(monkeypatch)
    client.get_series.return_value = {"seriess": [{"id": "GDP", "last_updated": "2024-01-01"}]}

    result = CliRunner().invoke(cli.app, ["health"])

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["details"]["series_id"] == "GDP"
