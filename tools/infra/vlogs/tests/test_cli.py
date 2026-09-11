from __future__ import annotations

from typer.testing import CliRunner
from vlogs import cli


def test_help_links_auth_failure_triage_skill() -> None:
    result = CliRunner().invoke(cli.app, ["--help"])

    assert result.exit_code == 0, result.output
    assert "auth-failure-log-triage" in result.output
    assert "centaur-skills" in result.output
