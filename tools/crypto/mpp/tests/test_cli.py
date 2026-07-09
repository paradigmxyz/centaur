from __future__ import annotations

import json

from mpp import cli
from typer.testing import CliRunner


class FakeClient:
    def list_services(self, **kwargs):
        assert kwargs == {"query": None, "category": "search", "tag": None, "limit": 5}
        return [{"id": "exa"}]

    def search_services(self, **kwargs):
        assert kwargs == {"query": "image", "category": None, "tag": None, "limit": 20}
        return [{"id": "fal"}]

    def get_service(self, service: str):
        assert service == "fal"
        return {"id": "fal", "endpoints": [{"payment": {"intent": "charge"}}]}


def test_service_commands_emit_json(monkeypatch) -> None:
    monkeypatch.setattr("mpp.client._client", lambda: FakeClient())
    runner = CliRunner()

    listed = runner.invoke(cli.app, ["services", "list", "--category", "search", "--limit", "5"])
    searched = runner.invoke(cli.app, ["services", "search", "image"])
    shown = runner.invoke(cli.app, ["services", "show", "fal"])

    assert listed.exit_code == 0, listed.output
    assert json.loads(listed.output)["services"] == [{"id": "exa"}]
    assert searched.exit_code == 0, searched.output
    assert json.loads(searched.output)["services"] == [{"id": "fal"}]
    assert shown.exit_code == 0, shown.output
    assert json.loads(shown.output)["endpoints"][0]["payment"] == {"intent": "charge"}


def test_service_commands_return_a_json_error(monkeypatch) -> None:
    class FailingClient:
        def get_service(self, service: str):
            raise ValueError(f"MPP service {service!r} was not found")

    monkeypatch.setattr("mpp.client._client", lambda: FailingClient())

    result = CliRunner().invoke(cli.app, ["services", "show", "missing"])

    assert result.exit_code == 1
    assert json.loads(result.output) == {"error": "MPP service 'missing' was not found"}
