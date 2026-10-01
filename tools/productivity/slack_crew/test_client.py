"""Run with uv run --with pytest --with httpx pytest test_client.py."""

import importlib.util
import json
from pathlib import Path

import httpx
import pytest

spec = importlib.util.spec_from_file_location("crew_client", Path(__file__).with_name("client.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def test_only_self_endpoint_and_no_admin_token(monkeypatch):
    monkeypatch.setenv("SLACK_CREW_ADMIN_TOKEN", "must-not-be-used")
    calls = []

    def respond(request):
        calls.append(request)
        return httpx.Response(200, json={"data": {"id": "research"}})

    client = module.SlackCrewClient("http://console", httpx.MockTransport(respond))
    assert client.me() == {"id": "research"}
    client.edit(description="")
    assert [request.method for request in calls] == ["GET", "PATCH"]
    assert all(request.url.path == "/api/v1/sandbox/crew/me" for request in calls)
    assert all("authorization" not in request.headers for request in calls)
    assert json.loads(calls[1].content) == {"data": {"description": ""}}
    with pytest.raises(ValueError):
        client.edit()
    assert len(calls) == 2


def test_does_not_follow_redirects_or_return_backend_error_body():
    def respond(request):
        return httpx.Response(302, headers={"Location": "http://other"}, text="private-detail")

    client = module.SlackCrewClient("http://console", httpx.MockTransport(respond))
    with pytest.raises(RuntimeError, match="HTTP 302") as error:
        client.me()
    assert "private-detail" not in str(error.value)
