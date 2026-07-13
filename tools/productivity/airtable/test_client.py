from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import httpx

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT))

from centaur_sdk import ToolContext, reset_tool_context, set_tool_context  # noqa: E402

CLIENT_PATH = REPO_ROOT / "tools" / "productivity" / "airtable" / "client.py"


def _load_airtable_module():
    spec = importlib.util.spec_from_file_location("test_airtable_client_module", CLIENT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _mock_client(client, handler) -> None:
    client._client.close()
    client._client = httpx.Client(
        transport=httpx.MockTransport(handler),
        headers={"Content-Type": "application/json"},
    )


def test_client_factory_loads_without_secret_and_preflight_reports_missing_secret() -> None:
    module = _load_airtable_module()
    token = set_tool_context(ToolContext(name="airtable", secrets={"AIRTABLE_API_KEY": ""}))
    try:
        client = module._client()
        result = client.preflight_access(
            url="https://airtable.com/appBase123/tblTable456/viwView789"
        )
        client.close()
    finally:
        reset_tool_context(token)

    assert result["status"] == "missing_secret"
    assert result["auth"]["attempted"] is False
    assert result["probe"]["attempted"] is False


def test_preflight_access_reports_invalid_token_from_whoami() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == "/v0/meta/whoami"
        return httpx.Response(
            401,
            request=request,
            json={
                "error": {
                    "type": "AUTHENTICATION_REQUIRED",
                    "message": "Authentication required",
                }
            },
        )

    _mock_client(client, handler)
    try:
        result = client.preflight_access(base_id="appBase123")
    finally:
        client.close()

    assert result["status"] == "invalid_token"
    assert result["auth"]["status"] == "invalid_token"
    assert result["probe"]["status"] == "not_run"


def test_preflight_access_reports_missing_base_scope_from_probe() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "GET" and request.url.path == "/v0/meta/whoami":
            return httpx.Response(
                200, request=request, json={"id": "usr123", "scopes": ["data.records:read"]}
            )
        if request.method == "GET" and request.url.path == "/v0/appBase123/tblTable456":
            assert request.url.params.get("pageSize") == "1"
            return httpx.Response(
                403,
                request=request,
                json={
                    "error": {
                        "type": "INVALID_PERMISSIONS_OR_MODEL_NOT_FOUND",
                        "message": "Invalid permissions, or the requested model was not found.",
                    }
                },
            )
        raise AssertionError(f"unexpected request: {request.method} {request.url}")

    _mock_client(client, handler)
    try:
        result = client.preflight_access(base_id="appBase123", table="tblTable456")
    finally:
        client.close()

    assert result["status"] == "missing_base_scope"
    assert result["auth"]["status"] == "ok"
    assert result["probe"]["status"] == "missing_base_scope"
    assert result["auth"]["identity"] == {
        "identity_present": True,
        "id": "usr123",
        "scopes_count": 1,
    }
    assert "email" not in result["auth"]["identity"]


def test_preflight_access_rejects_non_airtable_urls_before_network_calls() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")
    try:
        result = client.preflight_access(url="https://example.com/not-airtable")
    finally:
        client.close()

    assert result["status"] == "bad_url"
    assert result["auth"]["attempted"] is False
    assert result["probe"]["attempted"] is False


def test_preflight_access_returns_ok_when_auth_and_read_probe_succeed() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "GET" and request.url.path == "/v0/meta/whoami":
            return httpx.Response(200, request=request, json={"id": "usr123"})
        if request.method == "GET" and request.url.path == "/v0/appBase123/tblTable456":
            assert request.url.params.get("pageSize") == "1"
            assert request.url.params.get("view") == "viwView789"
            return httpx.Response(
                200,
                request=request,
                json={
                    "records": [{"id": "rec1", "fields": {"Name": "Ada"}}],
                    "offset": "next-page",
                },
            )
        raise AssertionError(f"unexpected request: {request.method} {request.url}")

    _mock_client(client, handler)
    try:
        result = client.preflight_access(
            url="https://airtable.com/appBase123/tblTable456/viwView789"
        )
    finally:
        client.close()

    assert result["ok"] is True
    assert result["status"] == "ok"
    assert result["probe"]["type"] == "records"
    assert result["probe"]["details"] == {"record_count": 1, "has_more": True}


def test_create_record_posts_fields_and_compacts_response() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "POST"
        assert request.url.path == "/v0/appBase123/tblTable456"
        assert json.loads(request.content) == {
            "fields": {"Name": "Ada", "Done": True},
            "typecast": True,
        }
        return httpx.Response(
            200,
            request=request,
            json={
                "id": "rec1",
                "createdTime": "2026-07-13T00:00:00.000Z",
                "fields": {"Name": "Ada", "Done": True},
            },
        )

    _mock_client(client, handler)
    try:
        result = client.create_record(
            "appBase123",
            "tblTable456",
            {"Name": "Ada", "Done": True},
            typecast=True,
        )
    finally:
        client.close()

    assert result == {
        "id": "rec1",
        "createdTime": "2026-07-13T00:00:00.000Z",
        "fields": {"Name": "Ada", "Done": True},
    }


def test_update_records_batches_and_uses_patch() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")
    calls: list[list[dict]] = []

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "PATCH"
        assert request.url.path == "/v0/appBase123/tblTable456"
        body = json.loads(request.content)
        calls.append(body["records"])
        assert body["typecast"] is False
        return httpx.Response(200, request=request, json={"records": body["records"]})

    records = [{"id": f"rec{index}", "fields": {"Index": index}} for index in range(11)]
    _mock_client(client, handler)
    try:
        result = client.update_records("appBase123", "tblTable456", records)
    finally:
        client.close()

    assert [len(call) for call in calls] == [10, 1]
    assert result["count"] == 11
    assert result["records"][0] == {
        "id": "rec0",
        "createdTime": None,
        "fields": {"Index": 0},
    }


def test_upsert_records_sends_perform_upsert() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "PATCH"
        assert request.url.path == "/v0/appBase123/Table Name"
        assert json.loads(request.content) == {
            "records": [{"fields": {"External ID": "ext-1", "Name": "Ada"}}],
            "typecast": False,
            "performUpsert": {"fieldsToMergeOn": ["External ID"]},
        }
        return httpx.Response(
            200,
            request=request,
            json={
                "records": [{"id": "rec1", "fields": {"External ID": "ext-1", "Name": "Ada"}}],
                "createdRecords": ["rec1"],
                "updatedRecords": [],
            },
        )

    _mock_client(client, handler)
    try:
        result = client.upsert_records(
            "appBase123",
            "Table Name",
            [{"External ID": "ext-1", "Name": "Ada"}],
            fields_to_merge_on=["External ID"],
        )
    finally:
        client.close()

    assert result["createdRecords"] == ["rec1"]
    assert result["updatedRecords"] == []
    assert result["records"][0]["id"] == "rec1"


def test_delete_records_batches_query_params() -> None:
    module = _load_airtable_module()
    client = module.AirtableClient(api_key="test-key")
    calls: list[list[str]] = []

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "DELETE"
        assert request.url.path == "/v0/appBase123/tblTable456"
        record_ids = request.url.params.get_list("records[]")
        calls.append(record_ids)
        return httpx.Response(
            200,
            request=request,
            json={"records": [{"id": record_id, "deleted": True} for record_id in record_ids]},
        )

    _mock_client(client, handler)
    try:
        result = client.delete_records(
            "appBase123",
            "tblTable456",
            [f"rec{index}" for index in range(12)],
        )
    finally:
        client.close()

    assert [len(call) for call in calls] == [10, 2]
    assert result["count"] == 12
    assert result["records"][-1] == {"id": "rec11", "deleted": True}
