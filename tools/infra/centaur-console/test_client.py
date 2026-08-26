import json

import httpx
import pytest
from client import (
    SANDBOX_OAUTH_APPS_PATH,
    SANDBOX_PERMISSIONS_PATH,
    SANDBOX_SCHEDULED_TASKS_PATH,
    ConsoleClient,
)


def json_response(payload, status_code=200):
    return httpx.Response(status_code, json=payload)


def make_client(handler, *, bearer_token=None):
    return ConsoleClient(
        url="http://centaur-console:3000",
        bearer_token=bearer_token,
        transport=httpx.MockTransport(handler),
    )


def test_sandbox_permissions_fetches_and_unwraps_data():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == SANDBOX_PERMISSIONS_PATH
        assert request.headers["Accept"] == "application/json"
        return json_response(
            {
                "data": {
                    "sandbox_id": "sandbox-1",
                    "principal_id": "prn_123",
                    "permissions": {"secrets": []},
                }
            }
        )

    result = make_client(handler).sandbox_permissions()

    assert result["sandbox_id"] == "sandbox-1"
    assert result["principal_id"] == "prn_123"
    assert result["permissions"] == {"secrets": []}


def test_sandbox_permissions_sends_debug_bearer_token_when_provided():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["Authorization"] == "Bearer test-token"
        return json_response({"data": {"sandbox_id": "sandbox-1"}})

    assert make_client(handler, bearer_token="test-token").permissions()["sandbox_id"] == "sandbox-1"


def test_sandbox_permissions_wraps_http_errors():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler).sandbox_permissions()


def test_sandbox_oauth_apps_fetches_and_unwraps_data():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == SANDBOX_OAUTH_APPS_PATH
        assert request.headers["Accept"] == "application/json"
        return json_response(
            {
                "data": [
                    {
                        "slug": "google",
                        "provider": "google",
                        "start_url": "https://console.example/oauth/google/start",
                    }
                ]
            }
        )

    result = make_client(handler).sandbox_oauth_apps()

    assert result == [
        {
            "slug": "google",
            "provider": "google",
            "start_url": "https://console.example/oauth/google/start",
        }
    ]


def test_sandbox_oauth_apps_wraps_http_errors():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler).sandbox_oauth_apps()


def test_scheduled_tasks_list_and_read_owned_tasks():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.url.path == SANDBOX_SCHEDULED_TASKS_PATH:
            return json_response({"data": [{"id": "tsk_123", "name": "Daily briefing"}]})
        return json_response({"data": {"id": "tsk_123", "prompt": "Summarize updates."}})

    client = make_client(handler)
    tasks = client.scheduled_tasks()
    task = client.scheduled_task("tsk_123")

    assert tasks == [{"id": "tsk_123", "name": "Daily briefing"}]
    assert task["prompt"] == "Summarize updates."
    assert [request.method for request in requests] == ["GET", "GET"]
    assert requests[1].url.path == f"{SANDBOX_SCHEDULED_TASKS_PATH}/tsk_123"


def test_create_scheduled_task_posts_schedule_and_delivery():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "POST"
        assert request.url.path == SANDBOX_SCHEDULED_TASKS_PATH
        assert json.loads(request.content) == {
            "data": {
                "name": "Daily briefing",
                "prompt": "Summarize updates.",
                "cron_expression": "0 9 * * *",
                "delivery_channel": "dm",
                "enabled": True,
            }
        }
        return json_response({"data": {"id": "tsk_123", "delivery_channel": "U0123456789"}}, 201)

    result = make_client(handler).create_scheduled_task(
        name="Daily briefing",
        prompt="Summarize updates.",
        cron_expression="0 9 * * *",
    )

    assert result == {"id": "tsk_123", "delivery_channel": "U0123456789"}


def test_update_scheduled_task_patches_only_provided_fields():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "PATCH"
        assert request.url.path == f"{SANDBOX_SCHEDULED_TASKS_PATH}/tsk_123"
        assert json.loads(request.content) == {
            "data": {"name": "Morning briefing", "enabled": False}
        }
        return json_response(
            {"data": {"id": "tsk_123", "name": "Morning briefing", "enabled": False}}
        )

    result = make_client(handler).update_scheduled_task(
        "tsk_123",
        name="Morning briefing",
        enabled=False,
    )

    assert result["enabled"] is False


def test_update_scheduled_task_requires_a_field():
    with pytest.raises(ValueError, match="at least one scheduled task field"):
        make_client(lambda _request: json_response({})).update_scheduled_task("tsk_123")


def test_delete_and_run_scheduled_task_use_member_routes():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.method == "DELETE":
            return httpx.Response(204)
        return json_response({"data": {"id": "tsk_123", "queued": True}}, 202)

    client = make_client(handler)
    assert client.delete_scheduled_task("tsk_123") is None
    run = client.run_scheduled_task("tsk_123")

    assert run["queued"] is True
    assert [request.method for request in requests] == ["DELETE", "POST"]
    assert requests[1].url.path == f"{SANDBOX_SCHEDULED_TASKS_PATH}/tsk_123/run"


def test_scheduled_task_requests_wrap_http_errors():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "task not found"}}, status_code=404)

    with pytest.raises(RuntimeError, match="HTTP 404"):
        make_client(handler).scheduled_task("tsk_missing")


def test_health_returns_identity_details():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response(
            {
                "data": {
                    "sandbox_id": "sandbox-1",
                    "proxy_id": "proxy-1",
                    "principal_id": "principal-1",
                }
            }
        )

    result = make_client(handler).health()

    assert result == {
        "ok": True,
        "tool": "centaur-console",
        "error": None,
        "details": {
            "sandbox_id": "sandbox-1",
            "proxy_id": "proxy-1",
            "principal_id": "principal-1",
        },
    }
