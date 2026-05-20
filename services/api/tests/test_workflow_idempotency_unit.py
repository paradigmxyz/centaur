from __future__ import annotations

import uuid
from unittest.mock import AsyncMock, patch

import pytest


class _FakeWorkflowContext:
    def __init__(self, pool, run_id: str, run_input: dict):
        self._pool = pool
        self.run_id = run_id
        self.run_input = run_input

    def _peek_resolved_name(self, name: str) -> str:
        return name

    async def step(self, _name, fn, *, step_kind: str):
        assert step_kind == "agent_turn"
        return await fn()


@pytest.mark.asyncio
async def test_workflow_idempotency_mismatch_logs_safe_metadata():
    from api.runtime_control import ControlPlaneError
    from api.workflow_engine import _insert_workflow_run

    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    trigger_key = f"slack-turn:{uuid.uuid4().hex}"
    run_input = {
        "thread_key": thread_key,
        "message_id": trigger_key,
        "parts": [{"type": "text", "text": "changed"}],
        "history_messages": [{"message_id": "slack:prior", "parts": []}],
    }

    conn = AsyncMock()
    conn.fetchrow = AsyncMock(return_value={
        "run_id": "wfr_existing",
        "request_hash": "different-existing-request-hash",
    })

    with (
        patch("api.workflow_engine.get_workflow_handler", return_value=object()),
        patch("api.workflow_engine.log.warning") as warning,
    ):
        with pytest.raises(ControlPlaneError) as exc:
            await _insert_workflow_run(
                conn,
                workflow_name="slack_thread_turn",
                run_input=run_input,
                trigger_key=trigger_key,
                workflow_version="test",
                workflow_source_path=None,
                parent_run_id=None,
                root_run_id=None,
            )

    assert exc.value.code == "IDEMPOTENCY_PAYLOAD_MISMATCH"
    warning.assert_called_once()
    event_name = warning.call_args.args[0]
    fields = warning.call_args.kwargs
    assert event_name == "workflow_idempotency_payload_mismatch"
    assert fields["workflow_name"] == "slack_thread_turn"
    assert fields["trigger_key"] == trigger_key
    assert fields["thread_key"] == thread_key
    assert fields["input_keys"] == "history_messages,message_id,parts,thread_key"
    assert fields["run_id"] == "wfr_existing"
    assert fields["existing_request_hash_prefix"] == "different-ex"
    assert len(fields["request_hash_prefix"]) == 12


@pytest.mark.asyncio
async def test_agent_turn_skips_existing_history_message_before_append():
    from api.workflow_engine import SuspendWorkflow, do_agent_turn

    pool = AsyncMock()
    ctx = _FakeWorkflowContext(
        pool,
        "wfr_test",
        {
            "thread_key": "slack:C:thread",
            "history_messages": [
                {
                    "message_id": "slack:prior",
                    "parts": [{"type": "text", "text": "already stored"}],
                    "user_id": "U1",
                }
            ],
        },
    )

    append_message = AsyncMock(return_value={"ok": True})
    enqueue_execution = AsyncMock(return_value={
        "execution_id": "exe_test",
        "status": "queued",
    })

    with (
        patch("api.workflow_engine._compute_agent_session_title", new=AsyncMock(return_value=None)),
        patch("api.workflow_engine._compute_agent_session_header", new=AsyncMock(return_value=None)),
        patch("api.workflow_engine.slackbot_client.open_agent_session", new=AsyncMock(return_value=None)),
        patch("api.workflow_engine.spawn_assignment", new=AsyncMock(return_value={"assignment_generation": 1})),
        patch("api.workflow_engine._message_request_exists", new=AsyncMock(return_value=True)) as exists,
        patch("api.workflow_engine.append_message", new=append_message),
        patch("api.workflow_engine.enqueue_execution", new=enqueue_execution),
        patch("api.workflow_engine.get_execution", new=AsyncMock(return_value=None)),
    ):
        with pytest.raises(SuspendWorkflow):
            await do_agent_turn(
                ctx,
                parts=[{"type": "text", "text": "current"}],
                message_id="slack:current",
            )

    exists.assert_awaited_once_with(
        pool,
        thread_key="slack:C:thread",
        message_id="slack:prior",
    )
    append_message.assert_awaited_once()
    assert append_message.await_args.kwargs["message_id"] == "slack:current"
    enqueue_execution.assert_awaited_once()
