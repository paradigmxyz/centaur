import json
from unittest.mock import MagicMock

from centaur_tool_elfa import cli
from typer.testing import CliRunner


def test_event_summary_cli(monkeypatch):
    payload = {"success": True, "data": [{"summary": "ETF inflows rose.", "sourceLinks": []}]}
    client = MagicMock()
    client.__enter__.return_value = client
    client.get_event_summary.return_value = payload
    monkeypatch.setattr(cli, "_client", lambda: client)

    result = CliRunner().invoke(
        cli.app,
        ["event-summary", "ETH"],
    )

    assert result.exit_code == 0
    assert json.loads(result.stdout) == payload
    client.get_event_summary.assert_called_once_with("ETH", time_window="24h")
