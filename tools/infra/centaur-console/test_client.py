import json

import httpx
import pytest
from client import (
    SANDBOX_ADMIN_GRANTS_PATH,
    SANDBOX_ADMIN_PG_DSN_SECRETS_PATH,
    SANDBOX_ADMIN_PRINCIPALS_PATH,
    SANDBOX_ADMIN_ROLES_PATH,
    SANDBOX_ADMIN_SECRETS_PATH,
    SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH,
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


def test_create_scheduled_task_allows_an_omitted_schedule():
    def handler(request: httpx.Request) -> httpx.Response:
        assert json.loads(request.content)["data"]["cron_expression"] is None
        return json_response({"data": {"id": "tsk_123", "cron_expression": None}}, 201)

    result = make_client(handler).create_scheduled_task(
        name="One-off briefing",
        prompt="Summarize updates.",
    )

    assert result["cron_expression"] is None


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


def test_update_scheduled_task_clears_the_schedule_with_an_empty_cron():
    def handler(request: httpx.Request) -> httpx.Response:
        assert json.loads(request.content) == {"data": {"cron_expression": ""}}
        return json_response({"data": {"id": "tsk_123", "cron_expression": None}})

    result = make_client(handler).update_scheduled_task(
        "tsk_123",
        cron_expression="",
    )

    assert result["cron_expression"] is None


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


def test_admin_assign_role_resolves_foreign_ids_before_assigning():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.url.path == f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/lookup/slack-channel-c123":
            return json_response({"data": {"id": "prn_123"}})
        if request.url.path == f"{SANDBOX_ADMIN_ROLES_PATH}/lookup/tool-github":
            return json_response({"data": {"id": "role_456"}})
        assert request.method == "POST"
        assert request.url.path == f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/prn_123/roles"
        assert json.loads(request.content) == {"data": {"role_id": "role_456"}}
        return json_response({"data": {"id": "role_456", "foreign_id": "tool-github"}}, 201)

    result = make_client(handler).admin_assign_role("slack-channel-c123", "tool-github")

    assert result == {"id": "role_456", "foreign_id": "tool-github"}
    assert len(requests) == 3


def test_admin_roles_follows_pagination():
    def handler(request: httpx.Request) -> httpx.Response:
        page = int(request.url.params["page"])
        return json_response({"data": [{"id": f"role_{page}"}], "meta": {"total_pages": 2}})

    assert make_client(handler).admin_roles() == [{"id": "role_1"}, {"id": "role_2"}]


def test_admin_requests_surface_forbidden_errors():
    def handler(_request: httpx.Request) -> httpx.Response:
        message = "sandbox principal is not linked to an active Console admin"
        return json_response({"error": {"message": message}}, status_code=403)

    with pytest.raises(RuntimeError, match="HTTP 403"):
        make_client(handler).admin_unassign_role("prn_123", "role_456")


SECRETS = [
    {"id": "sst_1", "grant_key": "static_secret_id", "foreign_id": "github-pat", "name": "GitHub"},
    {"id": "pgd_2", "grant_key": "pg_dsn_secret_id", "foreign_id": "analytics", "name": "DB"},
]


def test_admin_grant_secret_posts_the_secret_under_its_grant_key():
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/prn_123":
            return json_response({"data": {"id": "prn_123"}})
        if request.url.path == SANDBOX_ADMIN_SECRETS_PATH:
            return json_response({"data": SECRETS})
        assert request.method == "POST"
        assert request.url.path == SANDBOX_ADMIN_GRANTS_PATH
        assert json.loads(request.content) == {
            "data": {"principal_id": "prn_123", "pg_dsn_secret_id": "pgd_2"}
        }
        return json_response({"data": {"id": "grt_9"}}, 201)

    assert make_client(handler).admin_grant_secret("prn_123", "analytics") == {"id": "grt_9"}


def test_admin_revoke_secret_deletes_the_matching_direct_grant():
    deleted = []

    def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "DELETE":
            deleted.append(request.url.path)
            return httpx.Response(204)
        if request.url.path == f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/prn_123":
            return json_response({"data": {"id": "prn_123"}})
        if request.url.path == SANDBOX_ADMIN_SECRETS_PATH:
            return json_response({"data": SECRETS})
        return json_response(
            {
                "data": [
                    {"id": "grt_1", "pg_dsn_secret_id": "pgd_other"},
                    {"id": "grt_2", "static_secret_id": "sst_1"},
                ],
                "meta": {"total_pages": 1},
            }
        )

    make_client(handler).admin_revoke_secret("prn_123", "sst_1")

    assert deleted == [f"{SANDBOX_ADMIN_GRANTS_PATH}/grt_2"]


def test_admin_grant_secret_refuses_ambiguous_names():
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == SANDBOX_ADMIN_SECRETS_PATH:
            duplicate = {**SECRETS[1], "id": "pgd_3", "foreign_id": "other"}
            return json_response({"data": [*SECRETS, duplicate]})
        return json_response({"data": {"id": "prn_123"}})

    with pytest.raises(RuntimeError, match="several secrets"):
        make_client(handler).admin_grant_secret("prn_123", "DB")


def test_admin_set_sandbox_access_sends_only_supplied_capabilities():
    def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "GET":
            return json_response({"data": {"id": "prn_123"}})
        assert request.method == "PATCH"
        assert request.url.path == f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/prn_123/sandbox_access"
        assert json.loads(request.content) == {"data": {"sandbox_sessions_read_enabled": True}}
        return json_response({"data": {"id": "prn_123"}})

    make_client(handler).admin_set_sandbox_access(
        "prn_123", sandbox_sessions_read_enabled=True, sandbox_repo_cache=None
    )


def test_admin_slack_external_teams_add_and_remove_one_team_id():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else None
        requests.append((request.method, request.url.path, body))
        return json_response({"data": ["T0PARTNER"]})

    client = make_client(handler)
    assert client.admin_add_slack_external_team("T0PARTNER") == ["T0PARTNER"]
    assert client.admin_remove_slack_external_team("E0GRID") == ["T0PARTNER"]

    assert requests == [
        ("POST", SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH, {"data": {"team_id": "T0PARTNER"}}),
        ("DELETE", f"{SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH}/E0GRID", None),
    ]


def test_admin_create_principal_posts_a_slack_channel_or_user_identity():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append((request.method, request.url.path, json.loads(request.content)))
        return json_response({"data": {"id": "prn_new"}}, 201)

    client = make_client(handler)
    assert client.admin_create_principal(slack_channel_id="C0NEW") == {"id": "prn_new"}
    client.admin_create_principal(slack_user_id="U0NEW")

    assert requests == [
        (
            "POST",
            SANDBOX_ADMIN_PRINCIPALS_PATH,
            {"data": {"kind": "slack_channel", "slack_channel_id": "C0NEW"}},
        ),
        (
            "POST",
            SANDBOX_ADMIN_PRINCIPALS_PATH,
            {"data": {"kind": "slack_dm", "slack_user_id": "U0NEW"}},
        ),
    ]


def test_admin_update_secret_patches_only_the_given_fields_at_its_kind_path():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else None
        requests.append((request.method, request.url.path, body))
        if request.url.path == SANDBOX_ADMIN_SECRETS_PATH:
            return json_response(
                {
                    "data": [
                        {"id": "pgs_1", "grant_key": "pg_dsn_secret_id", "foreign_id": "partner"}
                    ]
                }
            )
        return json_response({"data": {"id": "pgs_1"}})

    make_client(handler).admin_update_secret("partner", {"role": "writer"})

    assert requests[-1] == (
        "PATCH",
        f"{SANDBOX_ADMIN_PG_DSN_SECRETS_PATH}/pgs_1",
        {"data": {"role": "writer"}},
    )
