from __future__ import annotations

import importlib.util
import json
from pathlib import Path

import httpx
import pytest

spec = importlib.util.spec_from_file_location(
    "grafana_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
GrafanaClient = module.GrafanaClient


def _client(requests: list[httpx.Request], response: dict) -> GrafanaClient:
    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return httpx.Response(200, json=response)

    client = GrafanaClient(url="https://grafana.test", api_key="test")
    client._client = httpx.Client(base_url=client.base_url, transport=httpx.MockTransport(handler))
    return client


def test_save_dashboard_unwraps_get_envelope_and_keeps_folder() -> None:
    requests: list[httpx.Request] = []
    client = _client(requests, {"status": "success", "uid": "abc", "version": 4})
    envelope = {
        "meta": {"folderUid": "ops"},
        "dashboard": {"uid": "abc", "title": "API", "version": 3, "panels": []},
    }

    result = client.save_dashboard(envelope, message="tweak")

    assert result["version"] == 4
    assert requests[0].method == "POST"
    assert requests[0].url.path == "/api/dashboards/db"
    assert json.loads(requests[0].content) == {
        "dashboard": envelope["dashboard"],
        "overwrite": False,
        "folderUid": "ops",
        "message": "tweak",
    }


def test_save_dashboard_explicit_folder_and_overwrite() -> None:
    requests: list[httpx.Request] = []
    client = _client(requests, {"status": "success"})

    client.save_dashboard({"title": "New"}, folder_uid="team", overwrite=True)

    body = json.loads(requests[0].content)
    assert body == {"dashboard": {"title": "New"}, "overwrite": True, "folderUid": "team"}


def test_save_dashboard_requires_title() -> None:
    with pytest.raises(ValueError, match="title"):
        GrafanaClient(url="https://grafana.test").save_dashboard({"uid": "x"})


def test_delete_dashboard() -> None:
    requests: list[httpx.Request] = []
    client = _client(requests, {"message": "Dashboard deleted"})

    client.delete_dashboard("abc")

    assert requests[0].method == "DELETE"
    assert requests[0].url.path == "/api/dashboards/uid/abc"
