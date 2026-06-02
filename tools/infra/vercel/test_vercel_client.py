from __future__ import annotations

import importlib.util
from pathlib import Path
from typing import Any

spec = importlib.util.spec_from_file_location(
    "vercel_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
VercelClient = module.VercelClient


class RecordingVercelClient(VercelClient):
    def __init__(self) -> None:
        super().__init__(api_token="vercel-token")
        self.calls: list[dict[str, Any]] = []

    def _request(self, path: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.calls.append({"path": path, "params": params or {}})
        return {"ok": True}


def test_list_projects_maps_team_and_search_params() -> None:
    client = RecordingVercelClient()

    client.list_projects(search="phylax", team_id="team_123", limit=10)

    assert client.calls[-1] == {
        "path": "/v9/projects",
        "params": {
            "limit": 10,
            "search": "phylax",
            "from": None,
            "repoUrl": None,
            "teamId": "team_123",
        },
    }


def test_list_deployments_maps_filters() -> None:
    client = RecordingVercelClient()

    client.list_deployments(project_id="prj_123", state="READY", branch="main", sha="abc")

    params = client.calls[-1]["params"]
    assert client.calls[-1]["path"] == "/v6/deployments"
    assert params["projectId"] == "prj_123"
    assert params["state"] == "READY"
    assert params["gitSource.ref"] == "main"
    assert params["gitSource.sha"] == "abc"


def test_get_deployment_url_encodes_identifier() -> None:
    client = RecordingVercelClient()

    client.get_deployment("https://example.vercel.app", slug="phylax")

    assert client.calls[-1] == {
        "path": "/v13/deployments/https%3A%2F%2Fexample.vercel.app",
        "params": {"withGitRepoInfo": "true", "slug": "phylax"},
    }
