from __future__ import annotations

import json

import pytest
from centaur_tool_websearch import cli
from typer.testing import CliRunner

runner = CliRunner()


class FakeClient:
    def __init__(self) -> None:
        self.search_kwargs: dict = {}
        self.research_kwargs: dict = {}

    def _set_progress_callback(self, callback) -> None:
        self.progress = callback

    async def search(self, **kwargs):
        self.search_kwargs = kwargs
        return {
            "query": kwargs["query"],
            "results": [],
            "answer_markdown": None,
            "meta": {"backend": "tako:api", "partial_failures": []},
        }

    async def deep_research(self, **kwargs):
        self.research_kwargs = kwargs
        return {
            "question": kwargs["question"],
            "answer_markdown": "report",
            "sources": [],
            "iterations": [],
            "meta": {"backend": "tako:agent"},
        }


@pytest.fixture
def fake(monkeypatch: pytest.MonkeyPatch) -> FakeClient:
    client = FakeClient()
    monkeypatch.setattr(cli, "_client", lambda: client)
    return client


def test_search_effort_flag(fake: FakeClient) -> None:
    result = runner.invoke(cli.app, ["search", "US GDP", "--effort", "deep", "-n", "3"])
    assert result.exit_code == 0, result.output
    assert fake.search_kwargs["effort"] == "deep"
    assert fake.search_kwargs["mode"] is None
    assert fake.search_kwargs["num_results"] == 3
    assert json.loads(result.stdout)["meta"]["backend"] == "tako:api"


def test_search_mode_is_hidden_but_accepted(fake: FakeClient) -> None:
    result = runner.invoke(cli.app, ["search", "q", "--mode", "basic"])
    assert result.exit_code == 0, result.output
    assert fake.search_kwargs["mode"] == "basic"
    assert fake.search_kwargs["effort"] is None
    help_text = runner.invoke(cli.app, ["search", "--help"]).output
    assert "--effort" in help_text
    assert "--mode" not in help_text


def test_deep_research_effort_flag(fake: FakeClient) -> None:
    result = runner.invoke(cli.app, ["deep-research", "Why?", "--effort", "high", "--pretty"])
    assert result.exit_code == 0, result.output
    assert fake.research_kwargs["effort"] == "high"
    assert fake.research_kwargs["processor"] is None
    assert result.stdout.strip() == "report"


def test_deep_research_processor_is_hidden_but_accepted(fake: FakeClient) -> None:
    result = runner.invoke(cli.app, ["deep-research", "Why?", "--processor", "pro-fast"])
    assert result.exit_code == 0, result.output
    assert fake.research_kwargs["processor"] == "pro-fast"
    help_text = runner.invoke(cli.app, ["deep-research", "--help"]).output
    assert "--effort" in help_text
    assert "--processor" not in help_text


@pytest.mark.parametrize("command", [["search", "q"], ["deep-research", "q"], ["health"]])
def test_backend_misconfiguration_exits_cleanly(
    monkeypatch: pytest.MonkeyPatch, command: list[str]
) -> None:
    def unsupported():
        raise RuntimeError(
            "WEBSEARCH_BACKEND='exa' is not supported. Set it to one of: tako, parallel."
        )

    monkeypatch.setattr(cli, "_client", unsupported)

    result = runner.invoke(cli.app, command)

    assert result.exit_code == 1
    assert "is not supported" in result.output
    assert "Traceback" not in result.output
    assert result.exception is None or isinstance(result.exception, SystemExit)
