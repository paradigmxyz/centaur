from __future__ import annotations

import uuid
from unittest.mock import AsyncMock, patch

import pytest


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
