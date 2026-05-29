from __future__ import annotations

import datetime as dt
import json
import uuid

import httpx
import pytest


def _auth(api_key: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {api_key}"}


@pytest.mark.asyncio
async def test_final_delivery_lease_heartbeat_reclaim_and_retry_backoff(
    client,
    db_pool,
    api_key: str,
):
    platform = f"router-edge-{uuid.uuid4().hex}"
    retry_execution_id = f"exe-{uuid.uuid4().hex[:12]}"
    reclaim_execution_id = f"exe-{uuid.uuid4().hex[:12]}"
    thread_key = f"slack:C-router:{uuid.uuid4().hex}"

    await db_pool.execute(
        "INSERT INTO agent_final_delivery_outbox ("
        "execution_id, thread_key, delivery, state, final_payload, lease_owner, "
        "lease_expires_at, next_attempt_at, attempt_count"
        ") VALUES ($1, $2, $3::jsonb, 'sending', $4::jsonb, 'worker-a', "
        "NOW() + INTERVAL '1 minute', NOW(), 1)",
        retry_execution_id,
        thread_key,
        json.dumps({"platform": platform}),
        json.dumps({"result_text": "retry me"}),
    )
    await db_pool.execute(
        "INSERT INTO agent_final_delivery_outbox ("
        "execution_id, thread_key, delivery, state, final_payload, lease_owner, "
        "lease_expires_at, next_attempt_at, attempt_count"
        ") VALUES ($1, $2, $3::jsonb, 'sending', $4::jsonb, 'stale-worker', "
        "NOW() - INTERVAL '1 minute', NOW(), 2)",
        reclaim_execution_id,
        thread_key,
        json.dumps({"platform": platform}),
        json.dumps({"result_text": "reclaim me"}),
    )

    wrong_heartbeat = await client.post(
        f"/agent/final-deliveries/{retry_execution_id}/heartbeat",
        headers=_auth(api_key),
        json={"consumer_id": "worker-b", "lease_seconds": 30},
    )
    assert wrong_heartbeat.status_code == 409

    heartbeat = await client.post(
        f"/agent/final-deliveries/{retry_execution_id}/heartbeat",
        headers=_auth(api_key),
        json={"consumer_id": "worker-a", "lease_seconds": 30},
    )
    assert heartbeat.status_code == 200
    assert heartbeat.json() == {"ok": True, "execution_id": retry_execution_id}

    wrong_failure = await client.post(
        f"/agent/final-deliveries/{retry_execution_id}/failed",
        headers=_auth(api_key),
        json={
            "consumer_id": "worker-b",
            "error": "temporary outage",
            "retry_after_seconds": 1,
        },
    )
    assert wrong_failure.status_code == 409

    started_at = dt.datetime.now(dt.timezone.utc)
    failed = await client.post(
        f"/agent/final-deliveries/{retry_execution_id}/failed",
        headers=_auth(api_key),
        json={
            "consumer_id": "worker-a",
            "error": "temporary outage",
            "error_class": "slack_api",
            "retry_after_seconds": 1,
        },
    )
    assert failed.status_code == 200

    retry_row = await db_pool.fetchrow(
        "SELECT state, lease_owner, lease_expires_at, next_attempt_at, last_error "
        "FROM agent_final_delivery_outbox WHERE execution_id = $1",
        retry_execution_id,
    )
    assert retry_row is not None
    assert retry_row["state"] == "pending"
    assert retry_row["lease_owner"] is None
    assert retry_row["lease_expires_at"] is None
    assert retry_row["last_error"] == "slack_api: temporary outage"
    assert retry_row["next_attempt_at"] >= started_at + dt.timedelta(seconds=4)

    claim = await client.post(
        "/agent/final-deliveries/claim",
        headers=_auth(api_key),
        json={"consumer_id": "worker-b", "limit": 10, "platform": platform},
    )
    assert claim.status_code == 200
    assert claim.json()["deliveries"] == [
            {
                "execution_id": reclaim_execution_id,
                "thread_key": thread_key,
                "trace_id": None,
                "traceparent": None,
                "attempt_count": 3,
                "delivery": {"platform": platform},
                "final_payload": {"result_text": "reclaim me"},
            }
    ]

    reclaim_row = await db_pool.fetchrow(
        "SELECT state, lease_owner, attempt_count "
        "FROM agent_final_delivery_outbox WHERE execution_id = $1",
        reclaim_execution_id,
    )
    assert reclaim_row is not None
    assert reclaim_row["state"] == "sending"
    assert reclaim_row["lease_owner"] == "worker-b"
    assert reclaim_row["attempt_count"] == 3


@pytest.mark.asyncio
async def test_admin_api_key_create_list_revoke_and_revoked_auth_failure(
    client,
    managed_app,
    api_key: str,
):
    create_response = await client.post(
        "/admin/api-keys",
        headers=_auth(api_key),
        json={
            "name": f"router-edge-{uuid.uuid4().hex}",
            "scopes": ["agent"],
            "created_by": "pytest",
        },
    )
    assert create_response.status_code == 200
    created = create_response.json()
    plaintext_key = created["key"]
    assert plaintext_key.startswith("aiv2_")
    assert created["key_prefix"] == plaintext_key[:8]
    assert created["scopes"] == ["agent"]

    list_response = await client.get("/admin/api-keys", headers=_auth(api_key))
    assert list_response.status_code == 200
    listed_key = next(
        key for key in list_response.json()["keys"] if key["id"] == created["id"]
    )
    assert listed_key["name"] == created["name"]
    assert listed_key["active"] is True
    assert "key" not in listed_key
    assert "key_hash" not in listed_key

    transport = httpx.ASGITransport(
        app=managed_app,
        client=("198.51.100.10", 49152),
    )
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as external:
        valid_response = await external.get(
            "/agent/threads",
            headers=_auth(plaintext_key),
            params={"limit": 1},
        )
        assert valid_response.status_code == 200

        revoke_response = await client.delete(
            f"/admin/api-keys/{created['id']}",
            headers=_auth(api_key),
        )
        assert revoke_response.status_code == 200
        assert revoke_response.json() == {"status": "revoked", "id": created["id"]}

        revoked_response = await external.get(
            "/agent/threads",
            headers=_auth(plaintext_key),
            params={"limit": 1},
        )
        assert revoked_response.status_code == 401
        assert revoked_response.json()["detail"] == "Invalid API key"

    relist_response = await client.get("/admin/api-keys", headers=_auth(api_key))
    relisted_key = next(
        key for key in relist_response.json()["keys"] if key["id"] == created["id"]
    )
    assert relisted_key["active"] is False
    assert relisted_key["revoked_at"] is not None


@pytest.mark.asyncio
async def test_agent_threads_list_caps_message_previews(
    client,
    db_pool,
    api_key: str,
):
    thread_key = f"slack:C-preview:{uuid.uuid4().hex}"
    long_text = "x" * 10_000

    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, created_at) "
        "VALUES ($1, $2, 'user', $3::jsonb, NOW() - INTERVAL '1 minute')",
        f"msg-{uuid.uuid4().hex[:12]}",
        thread_key,
        json.dumps([{"type": "text", "text": long_text}]),
    )

    response = await client.get(
        "/agent/threads",
        headers=_auth(api_key),
        params={"limit": 1},
    )

    assert response.status_code == 200
    rows = response.json()["threads"]
    assert len(rows) == 1
    assert rows[0]["slack_thread_key"] == thread_key
    assert rows[0]["first_message"] == "x" * 500
    assert rows[0]["last_user_message"] == "x" * 500
    assert len(response.content) < 2_000


@pytest.mark.asyncio
async def test_workflow_router_list_get_children_checkpoints_cancel_and_events(
    client,
    db_pool,
    api_key: str,
):
    await db_pool.execute(
        "TRUNCATE TABLE workflow_events, workflow_checkpoints, workflow_runs CASCADE"
    )
    workflow_name = f"router_edge_{uuid.uuid4().hex}"
    parent_run_id = f"wfr-{uuid.uuid4().hex[:12]}"
    child_run_id = f"wfr-{uuid.uuid4().hex[:12]}"
    correlation_id = f"approval-{uuid.uuid4().hex}"
    thread_key = f"slack:C-workflow:{uuid.uuid4().hex}"

    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, workflow_source_path, request_hash, "
        "root_run_id, thread_key, status, input_json, started_at"
        ") VALUES ($1, $2, 'test-v1', 'tests', $3, $1, $4, 'waiting', $5::jsonb, NOW())",
        parent_run_id,
        workflow_name,
        f"hash-{parent_run_id}",
        thread_key,
        json.dumps({"parent": True}),
    )
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, workflow_source_path, request_hash, "
        "parent_run_id, root_run_id, thread_key, status, input_json"
        ") VALUES ($1, $2, 'test-v1', 'tests', $3, $4, $4, $5, 'queued', $6::jsonb)",
        child_run_id,
        workflow_name,
        f"hash-{child_run_id}",
        parent_run_id,
        thread_key,
        json.dumps({"child": True}),
    )
    await db_pool.execute(
        "INSERT INTO workflow_checkpoints ("
        "run_id, checkpoint_name, step_kind, execution_id, state"
        ") VALUES ($1, 'approval', 'event_wait', 'exe-linked-router', $2::jsonb)",
        parent_run_id,
        json.dumps(
            {
                "_waiting": True,
                "event_type": "approval",
                "correlation_id": correlation_id,
            }
        ),
    )

    list_response = await client.get(
        "/workflows/runs",
        headers=_auth(api_key),
        params={"workflow_name": workflow_name},
    )
    assert list_response.status_code == 200
    listed_ids = {item["run_id"] for item in list_response.json()["items"]}
    assert listed_ids == {parent_run_id, child_run_id}

    get_response = await client.get(
        f"/workflows/runs/{parent_run_id}",
        headers=_auth(api_key),
    )
    assert get_response.status_code == 200
    parent_body = get_response.json()
    assert parent_body["status"] == "waiting"
    assert parent_body["waiting_on"] == {
        "type": "event",
        "event_type": "approval",
        "correlation_id": correlation_id,
        "deadline": None,
    }
    assert parent_body["child_runs_count"] == 1

    checkpoints_response = await client.get(
        f"/workflows/runs/{parent_run_id}/checkpoints",
        headers=_auth(api_key),
    )
    assert checkpoints_response.status_code == 200
    checkpoints = checkpoints_response.json()["checkpoints"]
    assert checkpoints[0]["checkpoint_name"] == "approval"
    assert checkpoints[0]["step_kind"] == "event_wait"
    assert checkpoints[0]["execution_id"] == "exe-linked-router"
    assert checkpoints[0]["state"]["correlation_id"] == correlation_id

    children_response = await client.get(
        f"/workflows/runs/{parent_run_id}/children",
        headers=_auth(api_key),
    )
    assert children_response.status_code == 200
    assert [item["run_id"] for item in children_response.json()["items"]] == [
        child_run_id
    ]

    event_response = await client.post(
        "/workflows/events",
        headers=_auth(api_key),
        json={
            "event_type": "approval",
            "correlation_id": correlation_id,
            "payload": {"approved": True},
        },
    )
    assert event_response.status_code == 200
    assert event_response.json() == {
        "ok": True,
        "event_type": "approval",
        "correlation_id": correlation_id,
        "runs_woken": 1,
    }

    cancel_response = await client.post(
        f"/workflows/runs/{parent_run_id}/cancel",
        headers=_auth(api_key),
    )
    assert cancel_response.status_code == 200
    assert cancel_response.json()["status"] == "cancelled"


@pytest.mark.asyncio
async def test_agent_release_cancel_list_executions_and_thread_detail_contracts(
    client,
    db_pool,
    api_key: str,
):
    thread_key = f"slack:C-agent:{uuid.uuid4().hex}"
    runtime_id = f"rt-{uuid.uuid4().hex[:10]}"
    cancel_execution_id = f"exe-{uuid.uuid4().hex[:12]}"
    release_execution_id = f"exe-{uuid.uuid4().hex[:12]}"

    await db_pool.execute(
        "INSERT INTO agent_runtime_assignments ("
        "thread_key, assignment_generation, runtime_id, harness, engine, "
        "persona_id, prompt_ref, effective_agents_md_sha256, state"
        ") VALUES ($1, 1, $2, 'amp', 'amp', NULL, 'harness:amp', 'sha', 'active')",
        thread_key,
        runtime_id,
    )
    await db_pool.execute(
        "INSERT INTO sandbox_sessions ("
        "thread_key, sandbox_id, harness, engine, state, thread_name, agent_thread_id"
        ") VALUES ($1, $2, 'amp', 'amp', 'idle', 'Router edge thread', 'T-agent-thread')",
        thread_key,
        runtime_id,
    )
    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, user_id, parts, metadata, created_at) "
        "VALUES ($1, $2, 'user', 'U123', $3::jsonb, $4::jsonb, NOW() - INTERVAL '2 minutes')",
        f"msg-{uuid.uuid4().hex[:12]}",
        thread_key,
        json.dumps([{"type": "text", "text": "start this task"}]),
        json.dumps({"user_id": "U123", "user_name": "Alice"}),
    )
    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, metadata, created_at) "
        "VALUES ($1, $2, 'assistant', $3::jsonb, $4::jsonb, NOW() - INTERVAL '1 minute')",
        f"msg-{uuid.uuid4().hex[:12]}",
        thread_key,
        json.dumps([{"type": "text", "text": "working"}]),
        json.dumps(
            {
                "token_usage": {
                    "total_tokens": 9,
                    "input_tokens": 4,
                    "output_tokens": 5,
                    "cost_usd": 0.01,
                    "models": ["test-model"],
                }
            }
        ),
    )
    await db_pool.execute(
        "INSERT INTO agent_execution_requests ("
        "execution_id, thread_key, assignment_generation, execute_id, request_hash, "
        "status, delivery, metadata, created_at"
        ") VALUES ($1, $2, 1, 'exec-cancel-router', 'hash-cancel-router', "
        "'queued', '{}'::jsonb, $3::jsonb, NOW() - INTERVAL '30 seconds')",
        cancel_execution_id,
        thread_key,
        json.dumps({"purpose": "cancel"}),
    )
    await db_pool.execute(
        "INSERT INTO agent_execution_requests ("
        "execution_id, thread_key, assignment_generation, execute_id, request_hash, "
        "status, delivery, metadata, created_at"
        ") VALUES ($1, $2, 1, 'exec-release-router', 'hash-release-router', "
        "'queued', '{}'::jsonb, '{}'::jsonb, NOW())",
        release_execution_id,
        thread_key,
    )

    execution_response = await client.get(
        f"/agent/executions/{cancel_execution_id}",
        headers=_auth(api_key),
    )
    assert execution_response.status_code == 200
    execution_body = execution_response.json()
    assert execution_body["thread_key"] == thread_key
    assert execution_body["status"] == "queued"
    assert execution_body["agent_thread_id"] == "T-agent-thread"
    assert execution_body["metadata"] == {"purpose": "cancel"}

    list_response = await client.get(
        f"/agent/threads/{thread_key}/executions",
        headers=_auth(api_key),
        params={"limit": 1},
    )
    assert list_response.status_code == 200
    assert list_response.json()["thread_key"] == thread_key
    assert [item["execution_id"] for item in list_response.json()["executions"]] == [
        release_execution_id
    ]

    cancel_response = await client.post(
        f"/agent/executions/{cancel_execution_id}/cancel",
        headers=_auth(api_key),
    )
    assert cancel_response.status_code == 200
    assert cancel_response.json() == {
        "ok": True,
        "execution_id": cancel_execution_id,
        "thread_key": thread_key,
        "status": "cancelled",
    }

    detail_response = await client.get(
        "/agent/threads/detail",
        headers=_auth(api_key),
        params={"key": thread_key},
    )
    assert detail_response.status_code == 200
    detail = detail_response.json()
    assert detail["slack_thread_key"] == thread_key
    assert detail["harness"] == "amp"
    assert detail["state"] == "idle"
    assert detail["message_count"] == 2
    assert detail["last_user_message"] == "start this task"
    assert detail["participants"] == [
        {
            "id": "U123",
            "name": "Alice",
            "username": None,
            "avatar_url": None,
        }
    ]
    assert detail["token_usage"] == {
        "total_tokens": 9,
        "input_tokens": 4,
        "output_tokens": 5,
        "cost_usd": 0.01,
        "models": ["test-model"],
    }

    release_response = await client.post(
        f"/agent/threads/{thread_key}/release",
        headers=_auth(api_key),
        json={"release_id": f"rel-{uuid.uuid4().hex}", "cancel_inflight": True},
    )
    assert release_response.status_code == 200
    assert release_response.json() == {
        "ok": True,
        "thread_key": thread_key,
        "released": True,
        "assignment_generation": 1,
        "runtime_id": runtime_id,
    }

    release_row = await db_pool.fetchrow(
        "SELECT state FROM agent_runtime_assignments WHERE thread_key = $1",
        thread_key,
    )
    assert release_row is not None
    assert release_row["state"] == "released"
    release_execution_status = await db_pool.fetchval(
        "SELECT status FROM agent_execution_requests WHERE execution_id = $1",
        release_execution_id,
    )
    assert release_execution_status == "cancelled"
