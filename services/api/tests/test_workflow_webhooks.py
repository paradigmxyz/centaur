from __future__ import annotations

import hashlib
import hmac
import json
import uuid

import httpx
import pytest
import pytest_asyncio


@pytest_asyncio.fixture(autouse=True)
async def _clear_workflow_webhook_tables(db_pool):
    await db_pool.execute(
        "TRUNCATE TABLE workflow_events, workflow_schedules, workflow_checkpoints, workflow_runs "
        "CASCADE",
    )
    yield


def _jsonb(value):
    return json.loads(value) if isinstance(value, str) else value


def _sha256_signature(secret: str, body: bytes) -> str:
    digest = hmac.new(secret.encode("utf-8"), body, hashlib.sha256).hexdigest()
    return f"sha256={digest}"


@pytest_asyncio.fixture
async def anonymous_client(managed_app):
    transport = httpx.ASGITransport(app=managed_app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        yield client


@pytest.mark.asyncio
async def test_no_auth_workflow_webhook_enqueues_run(
    anonymous_client,
    db_pool,
    monkeypatch,
    tmp_path,
):
    from api.webhooks import get_webhook_spec
    from api.workflow_engine import discover_workflow_handlers

    workflow_name = f"webhook_echo_{uuid.uuid4().hex}"
    slug = f"echo-{uuid.uuid4().hex}"
    workflow_file = tmp_path / "webhook_echo.py"
    workflow_file.write_text(
        "WORKFLOW_NAME = " + repr(workflow_name) + "\n"
        "WEBHOOKS = [{\n"
        "    'slug': " + repr(slug) + ",\n"
        "    'auth': 'none',\n"
        "    'provider': 'test',\n"
        "    'trigger_key': {'type': 'header', 'header': 'X-Test-Delivery'},\n"
        "}]\n"
        "async def handler(inp, ctx):\n"
        "    return {'received': inp['webhook']['body']}\n",
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    registered = get_webhook_spec(slug)
    assert registered is not None
    assert registered.workflow_name == workflow_name

    response = await anonymous_client.post(
        f"/api/webhooks/{slug}?source=unit",
        headers={
            "Cookie": "session=should-not-persist",
            "X-Test-Delivery": "delivery-1",
        },
        json={"hello": "world"},
    )

    assert response.status_code == 202
    body = response.json()
    assert body["ok"] is True
    assert body["workflow_name"] == workflow_name
    assert body["status"] == "queued"
    assert body["idempotent"] is False

    run_row = await db_pool.fetchrow(
        "SELECT workflow_name, trigger_key, input_json FROM workflow_runs WHERE run_id = $1",
        body["run_id"],
    )
    assert run_row is not None
    assert run_row["workflow_name"] == workflow_name
    assert run_row["trigger_key"] == f"webhook:{slug}:x-test-delivery:delivery-1"

    run_input = _jsonb(run_row["input_json"])
    event = run_input["webhook"]
    assert event["slug"] == slug
    assert event["provider"] == "test"
    assert event["method"] == "POST"
    assert event["path"] == f"/api/webhooks/{slug}"
    assert event["query"] == {"source": "unit"}
    assert event["body"] == {"hello": "world"}
    assert "raw_body_sha256" in event
    assert "cookie" not in event["headers"]
    assert event["headers"]["x-test-delivery"] == "delivery-1"

    retry = await anonymous_client.post(
        f"/api/webhooks/{slug}?source=unit",
        headers={"X-Test-Delivery": "delivery-1"},
        json={"hello": "world"},
    )
    assert retry.status_code == 200
    assert retry.json()["run_id"] == body["run_id"]
    assert retry.json()["idempotent"] is True


@pytest.mark.asyncio
async def test_hmac_workflow_webhook_enqueues_run_with_valid_signature(
    anonymous_client,
    db_pool,
    monkeypatch,
    tmp_path,
):
    from api.workflow_engine import discover_workflow_handlers

    secret = "test-webhook-secret"
    monkeypatch.setenv("TEST_WEBHOOK_SECRET", secret)
    workflow_name = f"webhook_hmac_{uuid.uuid4().hex}"
    slug = f"hmac-{uuid.uuid4().hex}"
    workflow_file = tmp_path / "webhook_hmac.py"
    workflow_file.write_text(
        "WORKFLOW_NAME = " + repr(workflow_name) + "\n"
        "WEBHOOKS = [{\n"
        "    'slug': " + repr(slug) + ",\n"
        "    'auth': {\n"
        "        'type': 'hmac',\n"
        "        'secret_ref': 'TEST_WEBHOOK_SECRET',\n"
        "        'signature_header': 'X-Test-Signature',\n"
        "        'signature_prefix': 'sha256=',\n"
        "    },\n"
        "}]\n"
        "async def handler(inp, ctx):\n"
        "    return {'received': inp['webhook']['body']}\n",
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    raw_body = b'{"hello":"signed"}'
    response = await anonymous_client.post(
        f"/api/webhooks/{slug}",
        content=raw_body,
        headers={
            "Content-Type": "application/json",
            "X-Test-Signature": _sha256_signature(secret, raw_body),
        },
    )

    assert response.status_code == 202
    body = response.json()
    assert body["workflow_name"] == workflow_name

    run_row = await db_pool.fetchrow(
        "SELECT input_json FROM workflow_runs WHERE run_id = $1",
        body["run_id"],
    )
    run_input = _jsonb(run_row["input_json"])
    assert run_input["webhook"]["body"] == {"hello": "signed"}
    assert "x-test-signature" not in run_input["webhook"]["headers"]


@pytest.mark.asyncio
async def test_workflow_webhook_parses_form_payload_json(
    anonymous_client,
    db_pool,
    monkeypatch,
    tmp_path,
):
    from api.workflow_engine import discover_workflow_handlers

    secret = "test-webhook-secret"
    monkeypatch.setenv("TEST_WEBHOOK_SECRET", secret)
    workflow_name = f"webhook_form_{uuid.uuid4().hex}"
    slug = f"form-{uuid.uuid4().hex}"
    workflow_file = tmp_path / "webhook_form.py"
    workflow_file.write_text(
        "WORKFLOW_NAME = " + repr(workflow_name) + "\n"
        "WEBHOOKS = [{\n"
        "    'slug': " + repr(slug) + ",\n"
        "    'auth': {\n"
        "        'type': 'hmac',\n"
        "        'secret_ref': 'TEST_WEBHOOK_SECRET',\n"
        "        'signature_header': 'X-Test-Signature',\n"
        "        'signature_prefix': 'sha256=',\n"
        "    },\n"
        "    'allowed_content_types': [\n"
        "        'application/json',\n"
        "        'application/x-www-form-urlencoded',\n"
        "    ],\n"
        "}]\n"
        "async def handler(inp, ctx):\n"
        "    return {'received': inp['webhook']['body']}\n",
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    raw_body = b'payload=%7B%22hello%22%3A%22form%22%7D'
    response = await anonymous_client.post(
        f"/api/webhooks/{slug}",
        content=raw_body,
        headers={
            "Content-Type": "application/x-www-form-urlencoded",
            "X-Test-Signature": _sha256_signature(secret, raw_body),
        },
    )

    assert response.status_code == 202
    body = response.json()
    run_row = await db_pool.fetchrow(
        "SELECT input_json FROM workflow_runs WHERE run_id = $1",
        body["run_id"],
    )
    run_input = _jsonb(run_row["input_json"])
    assert run_input["webhook"]["body"] == {"hello": "form"}


@pytest.mark.asyncio
async def test_hmac_workflow_webhook_rejects_invalid_signature_without_run(
    anonymous_client,
    db_pool,
    monkeypatch,
    tmp_path,
):
    from api.workflow_engine import discover_workflow_handlers

    monkeypatch.setenv("TEST_WEBHOOK_SECRET", "test-webhook-secret")
    slug = f"hmac-reject-{uuid.uuid4().hex}"
    workflow_file = tmp_path / "webhook_hmac_reject.py"
    workflow_file.write_text(
        "WORKFLOW_NAME = 'webhook_hmac_reject'\n"
        "WEBHOOKS = [{\n"
        f"    'slug': {slug!r},\n"
        "    'auth': {'type': 'hmac', 'secret_ref': 'TEST_WEBHOOK_SECRET'},\n"
        "}]\n"
        "async def handler(inp, ctx):\n"
        "    return {'ok': True}\n",
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    response = await anonymous_client.post(
        f"/api/webhooks/{slug}",
        content=b'{"hello":"bad-signature"}',
        headers={
            "Content-Type": "application/json",
            "X-Webhook-Signature": "sha256=bad",
        },
    )

    assert response.status_code == 401
    run_count = await db_pool.fetchval(
        "SELECT COUNT(*)::int FROM workflow_runs WHERE workflow_name = 'webhook_hmac_reject'",
    )
    assert run_count == 0


@pytest.mark.asyncio
async def test_workflow_webhook_rejects_unregistered_slug(client):
    response = await client.post("/api/webhooks/missing-webhook", json={"hello": "world"})
    assert response.status_code == 404


@pytest.mark.asyncio
async def test_workflow_webhook_rejects_disallowed_method(client, monkeypatch, tmp_path):
    from api.workflow_engine import discover_workflow_handlers

    slug = f"post-only-{uuid.uuid4().hex}"
    workflow_file = tmp_path / "webhook_post_only.py"
    workflow_file.write_text(
        "WORKFLOW_NAME = 'webhook_post_only'\n"
        f"WEBHOOKS = [{{'slug': {slug!r}, 'auth': 'none'}}]\n"
        "async def handler(inp, ctx):\n"
        "    return {'ok': True}\n",
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    response = await client.get(f"/api/webhooks/{slug}")
    assert response.status_code == 405


@pytest.mark.asyncio
async def test_workflow_webhook_rejects_unsupported_content_type(
    client,
    monkeypatch,
    tmp_path,
):
    from api.workflow_engine import discover_workflow_handlers

    slug = f"json-only-{uuid.uuid4().hex}"
    workflow_file = tmp_path / "webhook_json_only.py"
    workflow_file.write_text(
        "WORKFLOW_NAME = 'webhook_json_only'\n"
        f"WEBHOOKS = [{{'slug': {slug!r}, 'auth': 'none'}}]\n"
        "async def handler(inp, ctx):\n"
        "    return {'ok': True}\n",
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    response = await client.post(
        f"/api/webhooks/{slug}",
        content="hello",
        headers={"Content-Type": "text/plain"},
    )
    assert response.status_code == 400
