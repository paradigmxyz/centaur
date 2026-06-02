from __future__ import annotations

import importlib.util
from pathlib import Path
from typing import Any

import pytest

spec = importlib.util.spec_from_file_location(
    "railway_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
RailwayClient = module.RailwayClient


class FakeResponse:
    status_code = 200
    text = "{}"

    def __init__(self, payload: dict[str, Any]):
        self.payload = payload

    def json(self) -> dict[str, Any]:
        return self.payload


class FakeHttp:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    def post(self, url: str, headers: dict[str, str], json: dict[str, Any]) -> FakeResponse:
        self.calls.append({"url": url, "headers": headers, "json": json})
        return FakeResponse({"data": {"ok": True}})


class RecordingRailwayClient(RailwayClient):
    def __init__(self) -> None:
        super().__init__(api_token="account-token", project_token="project-token")
        self.fake_http = FakeHttp()

    @property
    def client(self) -> FakeHttp:
        return self.fake_http


def test_graphql_uses_account_bearer_header() -> None:
    client = RecordingRailwayClient()

    client.graphql("query { me { id } }")

    call = client.fake_http.calls[-1]
    assert call["headers"]["Authorization"] == "Bearer account-token"
    assert "Project-Access-Token" not in call["headers"]


def test_graphql_uses_project_access_token_header() -> None:
    client = RecordingRailwayClient()

    client.project_token_info()

    call = client.fake_http.calls[-1]
    assert call["headers"]["Project-Access-Token"] == "project-token"
    assert "Authorization" not in call["headers"]


def test_graphql_rejects_mutations() -> None:
    client = RecordingRailwayClient()

    with pytest.raises(ValueError, match="read-only"):
        client.graphql("mutation { projectDelete(id: \"p\") }")


def test_list_deployments_builds_input_payload() -> None:
    client = RecordingRailwayClient()

    client.list_deployments("project", service_id="service", environment_id="env", first=5)

    variables = client.fake_http.calls[-1]["json"]["variables"]
    assert variables == {
        "input": {
            "projectId": "project",
            "serviceId": "service",
            "environmentId": "env",
        },
        "first": 5,
    }
