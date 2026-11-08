"""Tests for the EDGAR CLI."""

from __future__ import annotations

import json
from unittest.mock import MagicMock

import httpx
from centaur_tool_edgar import cli
from typer.testing import CliRunner


def _mock_client(monkeypatch) -> MagicMock:
    client = MagicMock()
    client.__enter__.return_value = client
    client.user_agent = "Test Agent test@example.com"
    monkeypatch.setattr(cli, "_client", lambda: client)
    return client


def test_filings_passes_parsed_options(monkeypatch):
    client = _mock_client(monkeypatch)
    client.filings.return_value = {"cik": 320193, "filings": []}

    result = CliRunner().invoke(
        cli.app, ["filings", "AAPL", "--form", "10-K, 10-Q", "--since", "2023-01-01", "-n", "5"]
    )

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["cik"] == 320193
    client.filings.assert_called_once_with(
        "AAPL", forms=["10-K", "10-Q"], since="2023-01-01", until=None, limit=5
    )


def test_health_reports_failure_without_response_body(monkeypatch):
    client = _mock_client(monkeypatch)
    request = httpx.Request("GET", "https://data.sec.gov/submissions/CIK0000320193.json")
    response = httpx.Response(403, request=request, text="sensitive response body")
    client.submissions.side_effect = httpx.HTTPStatusError(
        "forbidden", request=request, response=response
    )

    result = CliRunner().invoke(cli.app, ["health"])

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert "HTTP 403" in payload["error"]
    assert "EDGAR_USER_AGENT" in payload["error"]
    assert "sensitive response body" not in result.output


def test_lookup_error_exits_nonzero(monkeypatch):
    client = _mock_client(monkeypatch)
    client.facts.side_effect = LookupError("Unknown ticker 'NOPE'")

    result = CliRunner().invoke(cli.app, ["facts", "NOPE", "us-gaap:Revenues"])

    assert result.exit_code == 1
    assert "Unknown ticker" in result.output


def test_get_writes_full_text_to_file(monkeypatch, tmp_path):
    client = _mock_client(monkeypatch)
    client.document.return_value = {"accession": "a", "text": "full text", "truncated": False}
    out = tmp_path / "doc.txt"

    result = CliRunner().invoke(cli.app, ["get", "AAPL", "0000320193-24-000123", "-o", str(out)])

    assert result.exit_code == 0, result.output
    assert out.read_text() == "full text"
    assert "text" not in json.loads(result.stdout)
