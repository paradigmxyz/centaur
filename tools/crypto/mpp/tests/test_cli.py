from __future__ import annotations

import json

from mpp import cli
from typer.testing import CliRunner


class FakeClient:
    def list_services(self, **kwargs):
        assert kwargs == {"query": None, "category": "search", "tag": None, "limit": 5}
        return {"services": [{"id": "exa"}], "cache": {"stale": False}}

    def search_services(self, **kwargs):
        assert kwargs == {"query": "image", "category": None, "tag": None, "limit": 20}
        return {"services": [{"id": "fal"}], "cache": {"stale": False}}

    def show_service(self, service: str):
        assert service == "fal"
        return {"id": "fal", "endpoints": [{"payment": {"intent": "charge"}}]}

    def request(self, **kwargs):
        assert kwargs == {
            "service": "fal",
            "method": "GET",
            "path": "/models/:id",
            "path_params": {"id": "fast"},
            "query": {"size": 1},
            "body": None,
        }
        return {"service": "fal", "status": 200, "data": {"ok": True}}

    def health(self):
        return {"ok": True, "service_count": 2}


def test_commands_emit_json(monkeypatch) -> None:
    monkeypatch.setattr("mpp.client._client", lambda: FakeClient())
    runner = CliRunner()

    listed = runner.invoke(cli.app, ["list", "--category", "search", "--limit", "5"])
    searched = runner.invoke(cli.app, ["search", "image"])
    shown = runner.invoke(cli.app, ["show", "fal"])
    requested = runner.invoke(
        cli.app,
        [
            "request",
            "fal",
            "--method",
            "GET",
            "--path",
            "/models/:id",
            "--path-params",
            '{"id":"fast"}',
            "--query",
            '{"size":1}',
        ],
    )
    health = runner.invoke(cli.app, ["health"])

    assert listed.exit_code == 0, listed.output
    assert json.loads(listed.output)["services"] == [{"id": "exa"}]
    assert searched.exit_code == 0, searched.output
    assert json.loads(searched.output)["services"] == [{"id": "fal"}]
    assert shown.exit_code == 0, shown.output
    assert json.loads(shown.output)["endpoints"][0]["payment"] == {"intent": "charge"}
    assert json.loads(requested.output)["data"] == {"ok": True}
    assert json.loads(health.output)["ok"] is True


def test_commands_return_json_errors(monkeypatch) -> None:
    class FailingClient:
        def show_service(self, service: str):
            raise ValueError(f"MPP service {service!r} was not found")

    monkeypatch.setattr("mpp.client._client", lambda: FailingClient())

    result = CliRunner().invoke(cli.app, ["show", "missing"])

    assert result.exit_code == 1
    assert json.loads(result.output) == {"error": "MPP service 'missing' was not found"}


def test_request_rejects_non_object_query(monkeypatch) -> None:
    monkeypatch.setattr("mpp.client._client", lambda: FakeClient())

    result = CliRunner().invoke(
        cli.app,
        ["request", "fal", "--method", "GET", "--path", "/models", "--query", "[]"],
    )

    assert result.exit_code == 2
    assert "--query must be a JSON object" in result.output
