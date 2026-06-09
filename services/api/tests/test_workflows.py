from __future__ import annotations

import base64
import datetime as dt
import json
import os
import uuid
from unittest.mock import AsyncMock, patch

import pytest
import pytest_asyncio


def _auth(api_key: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {api_key}"}


@pytest_asyncio.fixture(autouse=True)
async def _clear_workflow_tables(db_pool):
    await db_pool.execute(
        "TRUNCATE TABLE workflow_events, workflow_schedules, workflow_checkpoints, workflow_runs, "
        "agent_execution_events, agent_execution_requests, agent_final_delivery_outbox CASCADE",
    )
    yield


@pytest.mark.asyncio
async def test_create_slack_thread_turn_workflow_eager_start(
    client, db_pool, api_key: str,
):
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    payload = {
        "workflow_name": "slack_thread_turn",
        "trigger_key": f"slack-turn:{uuid.uuid4().hex}",
        "eager_start": True,
        "input": {
            "thread_key": thread_key,
            "parts": [{"type": "text", "text": "hello from workflow"}],
            "message_id": "slack:current",
            "user_id": "U123",
            "history_messages": [
                {
                    "message_id": "slack:prior",
                    "parts": [{"type": "text", "text": "prior context"}],
                    "user_id": "U123",
                },
                {
                    "message_id": "slack:assistant-prior",
                    "role": "assistant",
                    "parts": [{"type": "text", "text": "prior assistant context"}],
                },
                {
                    "message_id": "slack:current",
                    "parts": [{"type": "text", "text": "duplicate current should be skipped"}],
                    "user_id": "U123",
                },
            ],
            "delivery": {
                "platform": "slack",
                "channel": "C-test",
                "thread_ts": "1700000000.000100",
            },
        },
    }

    append_message_mock = AsyncMock(
        return_value={"ok": True, "message_id": "wf-msg"},
    )
    enqueue_execution_mock = AsyncMock(
        return_value={
            "ok": True,
            "execution_id": "exe-workflow-1",
            "status": "queued",
        },
    )
    calls: list[str] = []

    async def fake_insert_system_message(thread_key_arg, platform, *, user_id=None):
        calls.append("system")
        assert thread_key_arg == thread_key
        assert platform == "slack"
        assert user_id == "U123"

    async def fake_append_message(*args, **kwargs):
        calls.append("message")
        return {"ok": True, "message_id": "wf-msg"}

    append_message_mock.side_effect = fake_append_message

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(return_value={"assignment_generation": 7}),
        ),
        patch(
            "api.workflow_engine.append_message",
            new=append_message_mock,
        ),
        patch(
            "api.workflow_engine.enqueue_execution",
            new=enqueue_execution_mock,
        ),
        patch(
            "api.workflow_engine._insert_system_message",
            new=AsyncMock(side_effect=fake_insert_system_message),
        ),
        patch("api.workflow_engine._execute_run", new=AsyncMock()) as execute_run,
    ):
        response = await client.post(
            "/workflows/runs", headers=_auth(api_key), json=payload,
        )

    assert response.status_code == 200
    body = response.json()
    assert body["workflow_name"] == "slack_thread_turn"
    assert body["status"] == "queued"
    assert body["execution_id"] is None

    run_row = await db_pool.fetchrow(
        "SELECT workflow_name, status "
        "FROM workflow_runs WHERE run_id = $1",
        body["run_id"],
    )
    assert run_row is not None
    assert run_row["workflow_name"] == "slack_thread_turn"
    assert run_row["status"] == "queued"

    cp_row = await db_pool.fetchrow(
        "SELECT checkpoint_name, execution_id "
        "FROM workflow_checkpoints WHERE run_id = $1",
        body["run_id"],
    )
    assert cp_row is None
    append_message_mock.assert_not_awaited()
    enqueue_execution_mock.assert_not_awaited()
    execute_run.assert_not_awaited()
    assert calls == []


@pytest.mark.asyncio
async def test_slack_thread_turn_active_assignment_does_not_backfill_stop_history(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:T123:C123:{uuid.uuid4().hex}"
    nonce = f"CENTAUR_HISTORY_STOP_{uuid.uuid4().hex}"
    await db_pool.execute(
        "INSERT INTO agent_runtime_assignments ("
        "thread_key, assignment_generation, runtime_id, harness, engine, "
        "persona_id, prompt_ref, effective_agents_md_sha256, state"
        ") VALUES ($1, 3, $2, 'amp', 'amp', NULL, 'harness:amp', 'sha', 'active')",
        thread_key,
        f"rt-{uuid.uuid4().hex[:12]}",
    )
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-stop"})

    with patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "stop"}],
                message_id="slack:T123:C123:1778885070.000100",
                user_id="U123",
                history_messages=[
                    {
                        "message_id": "slack:T123:C123:1778885060.000100",
                        "parts": [
                            {
                                "type": "text",
                                "text": (
                                    f"Generate 60 haikus. Include {nonce} in every haiku. "
                                    "Number each haiku and write them in full."
                                ),
                            },
                        ],
                        "user_id": "U123",
                        "metadata": {"platform": "slack", "history_backfill": True},
                    },
                ],
            ),
            ctx,
        )

    do_agent_turn_mock.assert_awaited_once()
    assert do_agent_turn_mock.await_args.kwargs["parts"] == [{"type": "text", "text": "stop"}]
    assert do_agent_turn_mock.await_args.kwargs["history_messages"] == []


@pytest.mark.asyncio
async def test_slack_thread_turn_attachment_roundtrip_to_agent(
    client,
    db_pool,
    api_key: str,
):
    from api.deps import mint_sandbox_token
    from api.sandbox.harness_protocol import messages_to_content_blocks

    raw_attachment = b"%PDF-1.4 slack attachment bytes"
    thread_key = f"slack:T123:C123:{uuid.uuid4().hex}"
    message_id = f"slack:T123:C123:{uuid.uuid4().hex}"
    generation = 11

    async def fake_spawn(pool, *, thread_key: str, **_kwargs):
        await pool.execute(
            "INSERT INTO agent_runtime_assignments ("
            "thread_key, assignment_generation, runtime_id, harness, engine, "
            "persona_id, prompt_ref, effective_agents_md_sha256, state"
            ") VALUES ($1, $2, $3, 'amp', 'amp', NULL, 'harness:amp', 'sha', 'active')",
            thread_key,
            generation,
            f"rt-{uuid.uuid4().hex}",
        )
        return {"assignment_generation": generation}

    payload = {
        "workflow_name": "slack_thread_turn",
        "trigger_key": message_id,
        "eager_start": True,
        "input": {
            "thread_key": thread_key,
            "message_id": message_id,
            "user_id": "U123",
            "parts": [
                {"type": "text", "text": "please inspect this Slack file"},
                {
                    "type": "document",
                    "name": "customer-list.pdf",
                    "mime_type": "application/pdf",
                    "slack_file_id": "F123",
                    "source": {
                        "type": "base64",
                        "media_type": "application/pdf",
                        "data": base64.b64encode(raw_attachment).decode("utf-8"),
                    },
                },
            ],
            "delivery": {
                "platform": "slack",
                "channel": "C123",
                "thread_ts": "1778883099.579529",
                "recipient_user_id": "U123",
                "recipient_team_id": "T123",
            },
        },
    }

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(side_effect=fake_spawn),
        ),
        patch(
            "api.workflow_engine.enqueue_execution",
            new=AsyncMock(
                return_value={
                    "ok": True,
                    "execution_id": "exe-slack-attachment",
                    "status": "queued",
                }
            ),
        ),
    ):
        response = await client.post(
            "/workflows/runs",
            headers=_auth(api_key),
            json=payload,
        )

    assert response.status_code == 200
    body = response.json()
    assert body["status"] == "queued"

    from api.workflow_engine import _execute_run

    await _execute_run(db_pool, body["run_id"])

    attachment = await db_pool.fetchrow(
        "SELECT id, message_id, name, mime_type, data "
        "FROM attachments WHERE thread_key = $1",
        thread_key,
    )
    assert attachment is not None
    att_id = attachment["id"]
    assert attachment["name"] == "customer-list.pdf"
    assert attachment["mime_type"] == "application/pdf"
    assert bytes(attachment["data"]) == raw_attachment

    request_row = await db_pool.fetchrow(
        "SELECT event_json FROM agent_message_requests "
        "WHERE thread_key = $1 AND message_id = $2",
        thread_key,
        message_id,
    )
    assert request_row is not None
    event_json = request_row["event_json"]
    if isinstance(event_json, str):
        event_json = json.loads(event_json)
    stored_part = event_json["message"]["content"][1]
    assert stored_part["type"] == "attachment_ref"
    assert stored_part["attachment_id"] == att_id
    assert stored_part["name"] == "customer-list.pdf"
    assert "source" not in stored_part

    chat_row = await db_pool.fetchrow(
        "SELECT role, parts, user_id FROM chat_messages WHERE id = $1",
        attachment["message_id"],
    )
    assert chat_row is not None
    chat_parts = chat_row["parts"]
    if isinstance(chat_parts, str):
        chat_parts = json.loads(chat_parts)
    assert chat_parts[1] == {
        "type": "attachment_ref",
        "id": att_id,
        "name": "customer-list.pdf",
        "mime_type": "application/pdf",
    }

    blocks = messages_to_content_blocks(
        [
            {
                "role": chat_row["role"],
                "parts": chat_parts,
                "user_id": chat_row["user_id"],
            }
        ]
    )
    assert blocks[0]["text"] == "<@U123>: please inspect this Slack file"
    assert "User attached file: customer-list.pdf (application/pdf)" in blocks[1]["text"]
    assert f"/agent/attachments/{att_id}/download" in blocks[1]["text"]

    sandbox_token = mint_sandbox_token(thread_key, "rt-test")
    download = await client.get(
        f"/agent/attachments/{att_id}/download",
        headers={"Authorization": f"Bearer {sandbox_token}"},
    )
    assert download.status_code == 200
    assert download.content == raw_attachment


def test_recovery_command_paraphrases_are_recognized():
    """Real Slack utterances observed in production should trigger recovery
    hydration, not just the canonical 'retry' / 'continue' commands."""
    from api.workflows.slack_thread_turn import _is_recovery_turn

    paraphrases_observed_in_prod = [
        "again",
        "retry",
        "continue",
        "finish the job",
        "look at the root of this thread",
        "look at the root of this thread and try again",
        "look at the root of this thread, and try again",
        "Look at the root of this thread!",
        "reread the thread",
        "go again",
        "do it again",
        "<@U0AH5TRP0H0> again",
        "<@U0AH5TRP0H0> please continue",
    ]
    for text in paraphrases_observed_in_prod:
        assert _is_recovery_turn([{"type": "text", "text": text}]), (
            f"Expected recovery hydration for utterance: {text!r}"
        )

    not_recovery = [
        "retry the failing test in test_workflows.py",
        "continue editing the document",
        "look at the root cause of this bug",
        "let's go again to the office",
        "do it again but with the new params",
        "@U0AH5TRP0H0 again",
        "@Centaur AI again",
        "@Centaur AI, please retry",
        "@Centaur AI continue editing the document",
        "@Centaur AI thanks again",
        "@Centaur AI hey can you continue",
        "@U0AH5TRP0H0 thanks again",
        "@U0AH5TRP0H0 do it again but with the new params",
        "<@U0AH5TRP0H0> thanks again",
        "<@U0AH5TRP0H0> hey can you continue",
        "<@U0AH5TRP0H0> look at the root cause of this bug",
    ]
    for text in not_recovery:
        assert not _is_recovery_turn([{"type": "text", "text": text}]), (
            f"Did not expect recovery hydration for utterance: {text!r}"
        )


@pytest.mark.parametrize(
    ("text", "harness", "persona", "cleaned"),
    [
        ("--invest hyperliquid miqs", None, "invest", "hyperliquid miqs"),
        ("--INVEST hyperliquid miqs", None, "invest", "hyperliquid miqs"),
        ("\u2014invest hyperliquid miqs", None, "invest", "hyperliquid miqs"),
        ("\u2013invest hyperliquid miqs", None, "invest", "hyperliquid miqs"),
        ("`--invest` hyperliquid miqs", None, "invest", "hyperliquid miqs"),
        ("`--invest hyperliquid miqs`", None, "invest", "hyperliquid miqs"),
        ("--claude review this", "claude-code", None, "review this"),
        ("--pi analyze this", "pi-mono", None, "analyze this"),
        # Persona + harness compose orthogonally.
        ("--invest --claude review this", "claude-code", "invest", "review this"),
        ("--claude --invest review this", "claude-code", "invest", "review this"),
        ("--invest --amp review this", "amp", "invest", "review this"),
        ("--invest --codex review this", "codex", "invest", "review this"),
        ("please use --opus and review this", None, None, "please use and review this"),
        ("please use --model opus and review this", None, None, "please use and review this"),
        ("please use `--model opus` and review this", None, None, "please use and review this"),
    ],
)
def test_prompt_selection_extraction_handles_slack_flag_shapes(
    text, harness, persona, cleaned
):
    from api.workflows.slack_thread_turn import _extract_prompt_selection_from_text

    assert _extract_prompt_selection_from_text(
        text,
        personas={"invest"},
    ) == (harness, persona, cleaned)


def test_prompt_selection_extraction_preserves_unknown_flags():
    from api.workflows.slack_thread_turn import _extract_prompt_selection_from_text

    text = "--rpc-url https://example.test --installed"
    assert _extract_prompt_selection_from_text(
        text,
        personas={"invest"},
    ) == (None, None, text)


def test_bare_persona_flag_gets_intro_prompt():
    from api.workflows.slack_thread_turn import _extract_prompt_selection

    selection = _extract_prompt_selection(
        [{"type": "text", "text": "`--invest`"}],
        personas={"invest"},
    )

    assert selection.harness is None
    assert selection.persona == "invest"
    assert selection.parts == [
        {
            "type": "text",
            "text": (
                "Briefly introduce yourself using your active persona instructions and ask "
                "what we should work on."
            ),
        },
    ]


def test_persona_with_harness_keeps_user_text():
    from api.workflows.slack_thread_turn import _extract_prompt_selection

    selection = _extract_prompt_selection(
        [{"type": "text", "text": "--invest --claude review this PR"}],
        personas={"invest"},
    )

    assert selection.harness == "claude-code"
    assert selection.persona == "invest"
    assert selection.parts == [{"type": "text", "text": "review this PR"}]


def test_explicit_harness_and_persona_override_inline_flags():
    from api.workflows.slack_thread_turn import _extract_prompt_selection

    harness_only = _extract_prompt_selection(
        [{"type": "text", "text": "do the thing"}],
        explicit_harness="claude",
        personas={"invest"},
    )
    assert harness_only.harness == "claude-code"
    assert harness_only.persona is None

    persona_only = _extract_prompt_selection(
        [{"type": "text", "text": "do the thing"}],
        explicit_persona="invest",
        personas={"invest"},
    )
    assert persona_only.harness is None
    assert persona_only.persona == "invest"

    both = _extract_prompt_selection(
        [{"type": "text", "text": "do the thing"}],
        explicit_harness="amp",
        explicit_persona="invest",
        personas={"invest"},
    )
    assert both.harness == "amp"
    assert both.persona == "invest"


def test_prompt_switch_context_note_only_for_mid_thread_selector():
    from api.workflows.slack_thread_turn import _with_prompt_switch_context_note

    parts = [{"type": "text", "text": "pick this up"}]
    history = [{"message_id": "slack:prior", "parts": [{"type": "text", "text": "prior"}]}]

    assert _with_prompt_switch_context_note(parts, switched=False, history_messages=history) == parts
    assert _with_prompt_switch_context_note(parts, switched=True, history_messages=[]) == parts
    assert _with_prompt_switch_context_note(
        parts,
        switched=True,
        history_messages=history,
    ) == [
        {
            "type": "text",
            "text": (
                "You are being invoked mid-thread with a new active persona. Use the "
                "preceding Slack thread history as context, then answer the latest user "
                "request in that persona."
            ),
        },
        {"type": "text", "text": "pick this up"},
    ]


def test_workflow_idempotency_hash_ignores_history_messages():
    from api.workflow_engine import _workflow_request_hash

    base = {
        "thread_key": "slack:C:1",
        "message_id": "slack:1",
        "parts": [{"type": "text", "text": "current"}],
    }

    assert _workflow_request_hash("slack_thread_turn", {
        **base,
        "history_messages": [{"message_id": "slack:0", "parts": []}],
    }) == _workflow_request_hash("slack_thread_turn", {
        **base,
        "history_messages": [{"message_id": "slack:other", "parts": [{"type": "text", "text": "changed"}]}],
    })


def test_recovery_hydration_reads_workflow_history_messages():
    from api.workflows.slack_thread_turn import _lookup_last_unresolved_ask_from_history

    prior_ask, provenance = _lookup_last_unresolved_ask_from_history(
        [
            {
                "message_id": "slack:ask",
                "parts": [{"type": "text", "text": "Original ask from Slack history"}],
                "user_id": "U1",
            },
            {
                "message_id": "slack:retry-1",
                "parts": [{"type": "text", "text": "retry"}],
                "user_id": "U1",
            },
        ],
        user_id="U1",
        current_message_id="slack:retry-2",
    )

    assert prior_ask == "Original ask from Slack history"
    assert provenance["hydrated_from_message_id"] == "slack:ask"


@pytest.mark.asyncio
async def test_slack_thread_turn_hydrates_retry_with_last_substantive_user_ask(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"

    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, metadata, created_at) VALUES "
        "($1, $2, 'user', $3::jsonb, '{}'::jsonb, NOW() - INTERVAL '3 minutes'), "
        "($4, $2, 'assistant', $5::jsonb, '{}'::jsonb, NOW() - INTERVAL '2 minutes'), "
        "($6, $2, 'user', $7::jsonb, '{}'::jsonb, NOW() - INTERVAL '1 minute'), "
        "($8, $2, 'assistant', $9::jsonb, '{}'::jsonb, NOW())",
        f"msg:{thread_key}:ask",
        thread_key,
        json.dumps([{"type": "text", "text": "Build the storage access workflow and wire in the bucket credentials."}]),
        f"msg:{thread_key}:assistant-1",
        json.dumps([{"type": "text", "text": "I lost the earlier request."}]),
        f"msg:{thread_key}:retry-1",
        json.dumps([{"type": "text", "text": "retry"}]),
        f"msg:{thread_key}:assistant-2",
        json.dumps([{"type": "text", "text": "Paste the original request again."}]),
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})

    with patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock):
        result = await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "retry"}],
                message_id="msg-current",
            ),
            ctx,
        )

    assert result == {"ok": True, "execution_id": "exe-1"}
    hydrated_parts = do_agent_turn_mock.await_args.kwargs["parts"]
    assert hydrated_parts == [
        {
            "type": "text",
            "text": (
                "Previous unresolved user request from this thread:\n"
                "Build the storage access workflow and wire in the bucket credentials."
            ),
        },
        {"type": "text", "text": "retry"},
    ]


@pytest.mark.asyncio
async def test_slack_thread_turn_hydrates_slack_id_mention_prefixed_again(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"

    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, metadata, created_at) VALUES "
        "($1, $2, 'user', $3::jsonb, '{}'::jsonb, NOW() - INTERVAL '3 minutes'), "
        "($4, $2, 'assistant', $5::jsonb, '{}'::jsonb, NOW() - INTERVAL '2 minutes')",
        f"msg:{thread_key}:ask",
        thread_key,
        json.dumps([{"type": "text", "text": "Draft the partner update and include the shipping risks."}]),
        f"msg:{thread_key}:assistant-1",
        json.dumps([{"type": "text", "text": "I need the original request again."}]),
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})

    with patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "<@U0AH5TRP0H0> again"}],
                message_id="msg-current",
            ),
            ctx,
        )

    hydrated_parts = do_agent_turn_mock.await_args.kwargs["parts"]
    assert hydrated_parts == [
        {
            "type": "text",
            "text": (
                "Previous unresolved user request from this thread:\n"
                "Draft the partner update and include the shipping risks."
            ),
        },
        {"type": "text", "text": "<@U0AH5TRP0H0> again"},
    ]


@pytest.mark.asyncio
async def test_slack_thread_turn_recovery_filters_by_user_and_cursor(db_pool):
    """Recovery hydration must scope to the same user and only look back
    earlier than the triggering retry message."""
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    user_a = "U-alice"
    user_b = "U-bob"

    retry_msg_id = f"msg:{thread_key}:retry"
    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, metadata, user_id, created_at) VALUES "
        "($1, $2, 'user', $3::jsonb, '{}'::jsonb, $4, NOW() - INTERVAL '5 minutes'), "
        "($5, $2, 'user', $6::jsonb, '{}'::jsonb, $7, NOW() - INTERVAL '3 minutes'), "
        "($8, $2, 'user', $9::jsonb, '{}'::jsonb, $10, NOW() - INTERVAL '2 minutes'), "
        "($11, $2, 'user', $12::jsonb, '{}'::jsonb, $13, NOW() - INTERVAL '1 minute')",
        f"msg:{thread_key}:alice-original",
        thread_key,
        json.dumps([{"type": "text", "text": "Alice original ask: build the storage workflow"}]),
        user_a,
        f"msg:{thread_key}:bob-meta",
        json.dumps([{"type": "text", "text": "Bob unrelated request: rename the project to Mantle"}]),
        user_b,
        retry_msg_id,
        json.dumps([{"type": "text", "text": "retry"}]),
        user_a,
        f"msg:{thread_key}:bob-after",
        json.dumps([{"type": "text", "text": "Bob later: also fix the deploy script"}]),
        user_b,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})

    metadata: dict[str, object] = {}
    with patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "retry"}],
                message_id=retry_msg_id,
                user_id=user_a,
                metadata=metadata,
            ),
            ctx,
        )

    hydrated_parts = do_agent_turn_mock.await_args.kwargs["parts"]
    assert hydrated_parts[0]["text"] == (
        "Previous unresolved user request from this thread:\n"
        "Alice original ask: build the storage workflow"
    )
    assert hydrated_parts[1]["text"] == "retry"
    provenance = metadata.get("recovery_hydration") or {}
    assert provenance["hydrated_from_user_id"] == user_a
    assert provenance["hydrated_from_message_id"] == f"msg:{thread_key}:alice-original"


@pytest.mark.asyncio
async def test_slack_thread_turn_derives_persona_and_releases_assignment(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})
    release_assignment_mock = AsyncMock(return_value={"ok": True, "released": True})

    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch(
            "api.workflows.slack_thread_turn._known_personas",
            return_value={"invest"},
        ),
        patch("api.runtime_control.release_assignment", new=release_assignment_mock),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "--invest hyperliquid miqs"}],
                history_messages=[
                    {
                        "message_id": "slack:prior",
                        "parts": [{"type": "text", "text": "prior market context"}],
                    },
                ],
                message_id="slack:current",
            ),
            ctx,
        )

    release_assignment_mock.assert_awaited_once_with(
        db_pool,
        thread_key=thread_key,
        release_id="prompt-switch:slack:current",
        cancel_inflight=True,
        stop_runtime_background=True,
    )
    assert do_agent_turn_mock.await_args.kwargs["harness"] is None
    assert do_agent_turn_mock.await_args.kwargs["persona"] == "invest"
    assert do_agent_turn_mock.await_args.kwargs["parts"] == [
        {
            "type": "text",
            "text": (
                "You are being invoked mid-thread with a new active persona. Use the "
                "preceding Slack thread history as context, then answer the latest user "
                "request in that persona."
            ),
        },
        {"type": "text", "text": "hyperliquid miqs"},
    ]


@pytest.mark.asyncio
async def test_prompt_switch_retry_still_hydrates_prior_ask_from_history(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})

    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch(
            "api.workflows.slack_thread_turn._known_personas",
            return_value={"invest"},
        ),
        patch("api.runtime_control.release_assignment", new=AsyncMock()),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "--invest retry"}],
                history_messages=[
                    {
                        "message_id": "slack:prior",
                        "parts": [{"type": "text", "text": "Original investment ask"}],
                        "user_id": "U1",
                    },
                    {
                        "message_id": "slack:retry",
                        "parts": [{"type": "text", "text": "retry"}],
                        "user_id": "U1",
                    },
                ],
                message_id="slack:current",
                user_id="U1",
            ),
            ctx,
        )

    assert do_agent_turn_mock.await_args.kwargs["harness"] is None
    assert do_agent_turn_mock.await_args.kwargs["persona"] == "invest"
    assert do_agent_turn_mock.await_args.kwargs["parts"] == [
        {
            "type": "text",
            "text": (
                "You are being invoked mid-thread with a new active persona. Use the "
                "preceding Slack thread history as context, then answer the latest user "
                "request in that persona."
            ),
        },
        {
            "type": "text",
            "text": "Previous unresolved user request from this thread:\nOriginal investment ask",
        },
        {"type": "text", "text": "retry"},
    ]


@pytest.mark.asyncio
async def test_prompt_switch_clears_old_session_replay_state(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import _release_for_prompt_switch

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    await db_pool.execute(
        "INSERT INTO sandbox_sessions ("
        "thread_key, sandbox_id, harness, engine, state, agent_thread_id, "
        "last_delivered_id, inflight_turn_id, inflight_turn_input, inflight_attempts, "
        "last_result, last_result_at"
        ") VALUES ($1, 'sbx-old', 'codex', 'codex', 'stopped', 'old-agent-thread', "
        "'msg-old', 'turn-old', '{}'::jsonb, 2, 'old result', NOW())",
        thread_key,
    )
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    release_assignment_mock = AsyncMock(return_value={"ok": True, "released": True})

    with patch("api.runtime_control.release_assignment", new=release_assignment_mock):
        await _release_for_prompt_switch(
            ctx,
            thread_key=thread_key,
            message_id="slack:current",
        )

    release_assignment_mock.assert_awaited_once()
    row = await db_pool.fetchrow(
        "SELECT state, agent_thread_id, last_delivered_id, inflight_turn_id, inflight_turn_input, "
        "inflight_attempts, last_result, last_result_at "
        "FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    assert row is not None
    assert row["state"] == "stopped"
    assert row["agent_thread_id"] is None
    assert row["last_delivered_id"] is None
    assert row["inflight_turn_id"] is None
    assert row["inflight_turn_input"] is None
    assert row["inflight_attempts"] == 0
    assert row["last_result"] is None
    assert row["last_result_at"] is None


@pytest.mark.asyncio
async def test_slack_thread_turn_without_flag_keeps_default_harness_path(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})
    release_assignment_mock = AsyncMock(return_value={"ok": True, "released": True})

    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch(
            "api.workflows.slack_thread_turn._known_personas",
            return_value={"invest"},
        ),
        patch("api.runtime_control.release_assignment", new=release_assignment_mock),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "Summarize this thread"}],
                message_id="slack:current",
            ),
            ctx,
        )

    release_assignment_mock.assert_not_awaited()
    assert do_agent_turn_mock.await_args.kwargs["harness"] is None
    assert do_agent_turn_mock.await_args.kwargs["persona"] is None
    assert do_agent_turn_mock.await_args.kwargs["parts"] == [
        {"type": "text", "text": "Summarize this thread"},
    ]


@pytest.mark.asyncio
async def test_slack_thread_turn_without_flag_replays_history_only_for_new_assignment(db_pool):
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    history_messages = [
        {
            "message_id": "slack:prior",
            "parts": [{"type": "text", "text": "prior context"}],
            "user_id": "U1",
        },
    ]

    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})
    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch("api.runtime_control.get_active_assignment", new=AsyncMock(return_value=None)),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "Summarize this thread"}],
                history_messages=history_messages,
                message_id="slack:current",
            ),
            ctx,
        )

    assert do_agent_turn_mock.await_args.kwargs["history_messages"] == history_messages

    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-2"})
    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch(
            "api.runtime_control.get_active_assignment",
            new=AsyncMock(return_value={"assignment_generation": 1}),
        ),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "Continue"}],
                history_messages=history_messages,
                message_id="slack:next",
            ),
            ctx,
        )

    assert do_agent_turn_mock.await_args.kwargs["history_messages"] == []


@pytest.mark.asyncio
async def test_workflow_completes_when_execution_terminal(db_pool):
    from api.workflow_engine import _run_handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    execution_id = f"exe-{uuid.uuid4().hex[:12]}"

    # Insert a workflow run in "waiting" state
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "thread_key, status, input_json, worker_id"
        ") VALUES ($1, 'slack_thread_turn', 'test-slack-thread-turn-v1', 'hash', $1, $2, 'running', "
        "$3::jsonb, 'w1')",
        run_id,
        thread_key,
        json.dumps({
            "thread_key": thread_key,
            "parts": [{"type": "text", "text": "hello"}],
        }),
    )

    # Insert an existing checkpoint for the agent_turn step
    await db_pool.execute(
        "INSERT INTO workflow_checkpoints ("
        "run_id, checkpoint_name, step_kind, state, execution_id"
        ") VALUES ($1, 'agent_turn', 'agent_turn', $2::jsonb, $3)",
        run_id,
        json.dumps({
            "execution_id": execution_id,
            "status": "waiting",
        }),
        execution_id,
    )

    # Insert a completed execution
    await db_pool.execute(
        "INSERT INTO agent_execution_requests ("
        "execution_id, thread_key, assignment_generation, execute_id, "
        "request_hash, status, result_text, delivery, metadata"
        ") VALUES ($1, $2, 1, 'exec-1', 'hash', 'completed', "
        "'agent result text', '{}'::jsonb, '{}'::jsonb)",
        execution_id,
        thread_key,
    )

    # Simulate worker re-claiming and re-running the handler
    run_row = {
        "run_id": run_id,
        "workflow_name": "slack_thread_turn",
        "input_json": json.dumps({
            "thread_key": thread_key,
            "parts": [{"type": "text", "text": "hello"}],
        }),
        "status": "running",
        "worker_id": "w1",
    }
    await _run_handler(db_pool, run_row)

    # Verify the run completed
    result_row = await db_pool.fetchrow(
        "SELECT status, output_json FROM workflow_runs WHERE run_id = $1",
        run_id,
    )
    assert result_row is not None
    assert result_row["status"] == "completed"
    output = result_row["output_json"]
    if isinstance(output, str):
        output = json.loads(output)
    assert output["execution_id"] == execution_id


@pytest.mark.asyncio
async def test_checkpoint_replay_skips_fn(db_pool):
    """ctx.step() returns cached value without calling fn on replay."""
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    # Pre-populate a checkpoint
    await db_pool.execute(
        "INSERT INTO workflow_checkpoints ("
        "run_id, checkpoint_name, state"
        ") VALUES ($1, 'fetch', $2::jsonb)",
        run_id,
        json.dumps({"data": 42}),
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={"fetch": {"data": 42}},
        lease_s=30.0,
        worker_id="w1",
    )

    call_count = 0

    async def expensive_fn():
        nonlocal call_count
        call_count += 1
        return {"data": 99}

    result = await ctx.step("fetch", expensive_fn)
    assert result == {"data": 42}
    assert call_count == 0


@pytest.mark.asyncio
async def test_checkpoint_replay_preserves_none_result(db_pool):
    """A checkpointed None result is still treated as durable state."""
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )
    await db_pool.execute(
        "INSERT INTO workflow_checkpoints (run_id, checkpoint_name, state) "
        "VALUES ($1, 'noop', 'null'::jsonb)",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={"noop": None},
        lease_s=30.0,
        worker_id="w1",
    )

    call_count = 0

    async def should_not_run():
        nonlocal call_count
        call_count += 1
        return "unexpected"

    result = await ctx.step("noop", should_not_run)
    assert result is None
    assert call_count == 0


@pytest.mark.asyncio
async def test_eager_start_does_not_reexecute_existing_run(db_pool):
    from api.runtime_control import request_hash
    from api.workflow_engine import create_workflow_run

    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    trigger_key = f"slack-turn:{uuid.uuid4().hex}"
    run_input = {
        "thread_key": thread_key,
        "parts": [{"type": "text", "text": "hello from workflow"}],
    }
    existing_run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, trigger_key, root_run_id, thread_key, "
        "status, input_json"
        ") VALUES ($1, 'slack_thread_turn', 'test-slack-thread-turn-v1', $2, $3, $1, $4, 'waiting', $5::jsonb)",
        existing_run_id,
        request_hash({"workflow_name": "slack_thread_turn", "input": run_input}),
        trigger_key,
        thread_key,
        json.dumps(run_input),
    )

    with patch("api.workflow_engine._execute_run", new=AsyncMock()) as execute_run:
        result = await create_workflow_run(
            db_pool,
            workflow_name="slack_thread_turn",
            run_input=run_input,
            trigger_key=trigger_key,
            eager_start=True,
        )

    assert result["run_id"] == existing_run_id
    assert result["idempotent"] is True
    execute_run.assert_not_awaited()


@pytest.mark.asyncio
async def test_claim_run_requeues_expired_running_run(db_pool):
    from api.workflow_engine import _claim_run

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id, worker_lease_expires_at"
        ") VALUES ($1, 'multi_step_demo', 'test-multi-step-demo-v1', 'hash', $1, 'running', '{}'::jsonb, "
        "'stale-worker', NOW() - INTERVAL '5 minutes')",
        run_id,
    )

    claimed = await _claim_run(db_pool)

    assert claimed is not None
    assert claimed["run_id"] == run_id
    assert claimed["worker_id"] != "stale-worker"

    row = await db_pool.fetchrow(
        "SELECT status, worker_id FROM workflow_runs WHERE run_id = $1",
        run_id,
    )
    assert row is not None
    assert row["status"] == "running"
    assert row["worker_id"] == claimed["worker_id"]


@pytest.mark.asyncio
async def test_step_name_deduplication(db_pool):
    """Loop step names auto-deduplicate: fetch, fetch#2, fetch#3."""
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    results = []
    for i in range(3):
        val = i
        r = await ctx.step("fetch", lambda: {"i": val})
        results.append(r)

    # All three should have run and produced distinct checkpoints
    rows = await db_pool.fetch(
        "SELECT checkpoint_name FROM workflow_checkpoints "
        "WHERE run_id = $1 ORDER BY checkpoint_name",
        run_id,
    )
    names = [str(row["checkpoint_name"]) for row in rows]
    assert "fetch" in names
    assert "fetch#2" in names
    assert "fetch#3" in names


@pytest.mark.asyncio
async def test_agent_turn_idempotency_ids_follow_deduped_step_names(db_pool):
    from api.workflow_engine import SuspendWorkflow, WorkflowContext, do_agent_turn

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"workflow:{run_id}:gap-analysis:chunk-4"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'self_improve_daily', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    enqueue_execution_mock = AsyncMock(side_effect=[
        {"ok": True, "execution_id": "exe-first", "status": "queued"},
        {"ok": True, "execution_id": "exe-second", "status": "queued"},
    ])

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(return_value={"assignment_generation": 1}),
        ) as spawn_mock,
        patch("api.workflow_engine.append_message", new=AsyncMock()),
        patch("api.workflow_engine.enqueue_execution", new=enqueue_execution_mock),
    ):
        with pytest.raises(SuspendWorkflow):
            await do_agent_turn(
                ctx,
                prompt="review batch",
                thread_key=thread_key,
                message_id=f"wf:{run_id}:batch_review:chunk-4",
            )
        with pytest.raises(SuspendWorkflow):
            await do_agent_turn(
                ctx,
                prompt="repair batch",
                thread_key=thread_key,
                message_id=f"wf:{run_id}:batch_review:repair:chunk-4",
            )

    assert [
        call.kwargs["spawn_id"] for call in spawn_mock.await_args_list
    ] == [
        f"wf:{run_id}:agent_turn:spawn",
        f"wf:{run_id}:agent_turn#2:spawn",
    ]
    assert [
        call.kwargs["execute_id"] for call in enqueue_execution_mock.await_args_list
    ] == [
        f"wf:{run_id}:agent_turn:execute",
        f"wf:{run_id}:agent_turn#2:execute",
    ]

    rows = await db_pool.fetch(
        "SELECT checkpoint_name, execution_id "
        "FROM workflow_checkpoints WHERE run_id = $1 ORDER BY checkpoint_name",
        run_id,
    )
    indexed = {str(row["checkpoint_name"]): row["execution_id"] for row in rows}
    assert indexed == {
        "agent_turn": "exe-first",
        "agent_turn#2": "exe-second",
    }


@pytest.mark.asyncio
async def test_agent_turn_opens_slack_session_after_spawn_with_resolved_header(
    db_pool,
    monkeypatch,
):
    from api.workflow_engine import SuspendWorkflow, WorkflowContext, do_agent_turn

    monkeypatch.delenv("CODEX_MODEL", raising=False)
    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}:session-after-spawn"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    order: list[str] = []

    async def spawn_after_recording(*args, **kwargs):
        order.append("spawn")
        await db_pool.execute(
            "INSERT INTO agent_runtime_assignments ("
            "thread_key, assignment_generation, runtime_id, harness, engine, "
            "persona_id, prompt_ref, effective_agents_md_sha256, state"
            ") VALUES ($1, 1, 'rt-session-after-spawn', 'codex', 'codex', "
            "NULL, 'harness:codex', 'sha', 'active')",
            thread_key,
        )
        return {"assignment_generation": 1}

    async def open_after_spawn(**kwargs):
        order.append("open")
        assert kwargs["title"] == "Centaur · codex"
        assert kwargs["header"] == "base · codex"
        return "sess-resolved"

    append_message_mock = AsyncMock()
    enqueue_execution_mock = AsyncMock(
        return_value={"ok": True, "execution_id": "exe-resolved", "status": "queued"},
    )

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(side_effect=spawn_after_recording),
        ),
        patch(
            "api.workflow_engine.slackbot_client.open_agent_session",
            new=AsyncMock(side_effect=open_after_spawn),
        ) as open_session_mock,
        patch("api.workflow_engine.append_message", new=append_message_mock),
        patch("api.workflow_engine.enqueue_execution", new=enqueue_execution_mock),
    ):
        with pytest.raises(SuspendWorkflow):
            await do_agent_turn(
                ctx,
                prompt="hello",
                thread_key=thread_key,
                delivery={
                    "platform": "slack",
                    "channel": "C-test",
                    "thread_ts": "1700000000.000100",
                },
            )

    assert order == ["spawn", "open"]
    open_session_mock.assert_awaited_once()
    message_metadata = append_message_mock.await_args.kwargs["metadata"]
    assert message_metadata["slackbot_agent_session_id"] == "sess-resolved"
    assert message_metadata["slackbot_live_delivery"] is True
    enqueue_metadata = enqueue_execution_mock.await_args.kwargs["metadata"]
    assert enqueue_metadata["slackbot_agent_session_id"] == "sess-resolved"
    assert enqueue_metadata["slackbot_live_delivery"] is True


@pytest.mark.asyncio
async def test_agent_turn_omits_slack_session_header_after_prior_thread_execution(
    db_pool,
    monkeypatch,
):
    from api.workflow_engine import SuspendWorkflow, WorkflowContext, do_agent_turn

    monkeypatch.delenv("CODEX_MODEL", raising=False)
    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}:header-followup"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )
    await db_pool.execute(
        "INSERT INTO agent_execution_requests ("
        "execution_id, thread_key, assignment_generation, execute_id, request_hash, "
        "status, delivery, metadata"
        ") VALUES ($1, $2, 1, $3, 'prior-hash', 'completed', $4::jsonb, $5::jsonb)",
        f"exe-prior-{uuid.uuid4().hex[:8]}",
        thread_key,
        f"prior-execute-{uuid.uuid4().hex[:8]}",
        json.dumps({"platform": "slack"}),
        json.dumps({"slackbot_agent_session_id": "sess-prior"}),
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    async def spawn_after_recording(*args, **kwargs):
        await db_pool.execute(
            "INSERT INTO agent_runtime_assignments ("
            "thread_key, assignment_generation, runtime_id, harness, engine, "
            "persona_id, prompt_ref, effective_agents_md_sha256, state"
            ") VALUES ($1, 2, 'rt-header-followup', 'codex', 'codex', "
            "NULL, 'harness:codex', 'sha', 'active')",
            thread_key,
        )
        return {"assignment_generation": 2}

    async def open_after_spawn(**kwargs):
        assert kwargs["title"] == "Centaur · codex"
        assert kwargs["header"] is None
        return "sess-followup"

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(side_effect=spawn_after_recording),
        ),
        patch(
            "api.workflow_engine.slackbot_client.open_agent_session",
            new=AsyncMock(side_effect=open_after_spawn),
        ) as open_session_mock,
        patch("api.workflow_engine.append_message", new=AsyncMock()),
        patch(
            "api.workflow_engine.enqueue_execution",
            new=AsyncMock(
                return_value={
                    "ok": True,
                    "execution_id": "exe-followup",
                    "status": "queued",
                },
            ),
        ),
    ):
        with pytest.raises(SuspendWorkflow):
            await do_agent_turn(
                ctx,
                prompt="follow up",
                thread_key=thread_key,
                delivery={
                    "platform": "slack",
                    "channel": "C-test",
                    "thread_ts": "1700000000.000100",
                },
            )

    open_session_mock.assert_awaited_once()


@pytest.mark.asyncio
async def test_agent_turn_omits_slack_session_header_when_history_has_assistant_message(
    db_pool,
    monkeypatch,
):
    from api.workflow_engine import SuspendWorkflow, WorkflowContext, do_agent_turn

    monkeypatch.delenv("CODEX_MODEL", raising=False)
    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}:header-history"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    async def spawn_after_recording(*args, **kwargs):
        await db_pool.execute(
            "INSERT INTO agent_runtime_assignments ("
            "thread_key, assignment_generation, runtime_id, harness, engine, "
            "persona_id, prompt_ref, effective_agents_md_sha256, state"
            ") VALUES ($1, 1, 'rt-header-history', 'codex', 'codex', "
            "NULL, 'harness:codex', 'sha', 'active')",
            thread_key,
        )
        return {"assignment_generation": 1}

    async def open_after_spawn(**kwargs):
        assert kwargs["title"] == "Centaur · codex"
        assert kwargs["header"] is None
        return "sess-history"

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(side_effect=spawn_after_recording),
        ),
        patch(
            "api.workflow_engine.slackbot_client.open_agent_session",
            new=AsyncMock(side_effect=open_after_spawn),
        ) as open_session_mock,
        patch("api.workflow_engine.append_message", new=AsyncMock()),
        patch(
            "api.workflow_engine.enqueue_execution",
            new=AsyncMock(
                return_value={
                    "ok": True,
                    "execution_id": "exe-history",
                    "status": "queued",
                },
            ),
        ),
    ):
        with pytest.raises(SuspendWorkflow):
            await do_agent_turn(
                ctx,
                prompt="follow up",
                thread_key=thread_key,
                history_messages=[
                    {
                        "message_id": f"slack:C-test:{uuid.uuid4().hex}",
                        "role": "assistant",
                        "parts": [{"type": "text", "text": "Earlier answer."}],
                    },
                ],
                delivery={
                    "platform": "slack",
                    "channel": "C-test",
                    "thread_ts": "1700000000.000100",
                },
            )

    open_session_mock.assert_awaited_once()


@pytest.mark.asyncio
async def test_agent_turn_spawn_failure_opens_unresolved_failure_session(db_pool):
    from api.workflow_engine import WorkflowContext, do_agent_turn

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}:spawn-failure-session"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    append_message_mock = AsyncMock()
    enqueue_execution_mock = AsyncMock()

    with (
        patch(
            "api.workflow_engine.spawn_assignment",
            new=AsyncMock(side_effect=RuntimeError("spawn unavailable")),
        ),
        patch(
            "api.workflow_engine.slackbot_client.open_agent_session",
            new=AsyncMock(return_value="sess-failed"),
        ) as open_session_mock,
        patch(
            "api.workflow_engine.slackbot_client.session_text",
            new=AsyncMock(),
        ) as session_text_mock,
        patch(
            "api.workflow_engine.slackbot_client.session_done",
            new=AsyncMock(),
        ) as session_done_mock,
        patch("api.workflow_engine.append_message", new=append_message_mock),
        patch("api.workflow_engine.enqueue_execution", new=enqueue_execution_mock),
    ):
        with pytest.raises(RuntimeError, match="spawn unavailable"):
            await do_agent_turn(
                ctx,
                prompt="hello",
                thread_key=thread_key,
                delivery={
                    "platform": "slack",
                    "channel": "C-test",
                    "thread_ts": "1700000000.000100",
                },
            )

    open_session_mock.assert_awaited_once()
    open_kwargs = open_session_mock.await_args.kwargs
    assert open_kwargs["title"] == "Centaur"
    assert open_kwargs["header"] is None
    session_text_mock.assert_awaited_once_with(
        "sess-failed",
        "Failed to start the runtime: spawn unavailable",
    )
    session_done_mock.assert_awaited_once_with("sess-failed")
    append_message_mock.assert_not_awaited()
    enqueue_execution_mock.assert_not_awaited()


@pytest.mark.asyncio
async def test_step_only_auto_links_execution_ids_for_agent_turn_steps(db_pool):
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    await ctx.step(
        "review_json",
        lambda: {"execution_id": "exe-should-not-link", "value": 1},
        step_kind="review",
    )
    await ctx.step(
        "agent_dispatch",
        lambda: {"execution_id": "exe-should-link", "status": "waiting"},
        step_kind="agent_turn",
    )
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json"
        ") VALUES ($1, 'child', 'test-v1', 'hash-child', $1, 'queued', '{}'::jsonb)",
        "wfr_child_123",
    )
    await ctx.step(
        "child_start",
        lambda: {"run_id": "wfr_child_123", "status": "queued"},
        step_kind="child_workflow_start",
    )

    rows = await db_pool.fetch(
        "SELECT checkpoint_name, execution_id, child_run_id "
        "FROM workflow_checkpoints WHERE run_id = $1 ORDER BY checkpoint_name",
        run_id,
    )
    indexed = {str(row["checkpoint_name"]): row for row in rows}

    assert indexed["review_json"]["execution_id"] is None
    assert indexed["agent_dispatch"]["execution_id"] == "exe-should-link"
    assert indexed["child_start"]["child_run_id"] == "wfr_child_123"


@pytest.mark.asyncio
async def test_sleep_suspends_and_resumes(db_pool):
    """ctx.sleep() raises SuspendWorkflow; on replay after wake time
    it falls through."""
    from api.workflow_engine import WorkflowContext, SuspendWorkflow
    import datetime as _dt

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    # First call: should suspend
    with pytest.raises(SuspendWorkflow):
        await ctx.sleep("wait", _dt.timedelta(seconds=60))

    # Verify checkpoint was written
    cp = await db_pool.fetchrow(
        "SELECT state FROM workflow_checkpoints "
        "WHERE run_id = $1 AND checkpoint_name = 'wait'",
        run_id,
    )
    assert cp is not None

    # Replay with past wake time in checkpoint
    past = (_dt.datetime.now(_dt.timezone.utc) - _dt.timedelta(hours=1))
    ctx2 = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={"wait": past.isoformat()},
        lease_s=30.0,
        worker_id="w1",
    )
    # Should NOT raise — wake time is in the past
    await ctx2.sleep("wait", _dt.timedelta(seconds=60))


@pytest.mark.asyncio
async def test_notify_execution_terminal_wakes_run(db_pool):
    from api.workflow_engine import notify_execution_terminal

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    execution_id = f"exe-{uuid.uuid4().hex[:12]}"

    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, available_at"
        ") VALUES ($1, 'slack_thread_turn', 'test-slack-thread-turn-v1', 'hash', $1, 'waiting', "
        "'{}'::jsonb, '2099-01-01T00:00:00Z')",
        run_id,
    )
    await db_pool.execute(
        "INSERT INTO workflow_checkpoints ("
        "run_id, checkpoint_name, step_kind, state, execution_id"
        ") VALUES ($1, 'agent_turn', 'agent_turn', $2::jsonb, $3)",
        run_id,
        json.dumps({"execution_id": execution_id}),
        execution_id,
    )

    woke = await notify_execution_terminal(db_pool, execution_id)
    assert woke is True

    row = await db_pool.fetchrow(
        "SELECT available_at FROM workflow_runs WHERE run_id = $1",
        run_id,
    )
    # available_at should now be in the past (set to NOW())
    assert row["available_at"] <= dt.datetime.now(dt.timezone.utc)


@pytest.mark.asyncio
async def test_cancel_workflow_run_cancels_linked_execution(db_pool):
    from api.workflow_engine import cancel_workflow_run

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    execution_id = f"exe-{uuid.uuid4().hex[:12]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json"
        ") VALUES ($1, 'slack_thread_turn', 'test-slack-thread-turn-v1', 'hash', $1, 'waiting', "
        "'{}'::jsonb)",
        run_id,
    )
    await db_pool.execute(
        "INSERT INTO workflow_checkpoints ("
        "run_id, checkpoint_name, step_kind, state, execution_id"
        ") VALUES ($1, 'agent_turn', 'agent_turn', $2::jsonb, $3)",
        run_id,
        json.dumps({"execution_id": execution_id}),
        execution_id,
    )

    cancel_execution_mock = AsyncMock(return_value={"ok": True, "status": "cancelled"})
    with patch("api.workflow_engine.cancel_execution", new=cancel_execution_mock):
        result = await cancel_workflow_run(db_pool, run_id)

    assert result is not None
    assert result["status"] == "cancelled"
    cancel_execution_mock.assert_awaited_once_with(db_pool, execution_id)


@pytest.mark.asyncio
async def test_tick_workflow_schedules_is_idempotent(db_pool):
    from api.workflow_engine import _tick_workflow_schedules

    now = dt.datetime(2026, 3, 31, 14, 45, tzinfo=dt.timezone.utc)
    next_run_at = now
    await db_pool.execute(
        "INSERT INTO workflow_schedules ("
        "schedule_id, workflow_name, schedule_kind, schedule_expr, "
        "timezone, catchup_policy, input_json, enabled, next_run_at"
        ") VALUES ($1, 'slack_thread_turn', 'cron', '45 14 * * *', "
        "'UTC', 'skip', $2::jsonb, TRUE, $3)",
        "sched-test",
        json.dumps({
            "thread_key": f"slack:C-test:{uuid.uuid4().hex}",
            "parts": [{"type": "text", "text": "scheduled hello"}],
        }),
        next_run_at,
    )

    created_first = await _tick_workflow_schedules(db_pool, now=now)
    created_second = await _tick_workflow_schedules(db_pool, now=now)

    assert created_first == 1
    assert created_second == 0

    runs = await db_pool.fetch(
        "SELECT workflow_name, trigger_key FROM workflow_runs "
        "WHERE trigger_key = $1",
        f"schedule:sched-test:{int(next_run_at.timestamp())}",
    )
    assert len(runs) == 1


@pytest.mark.asyncio
async def test_sync_registered_workflow_schedules_disables_removed_rows(
    db_pool,
    monkeypatch,
):
    from api.workflow_engine import sync_registered_workflow_schedules

    for key in list(os.environ):
        if key.startswith("PRIVATE_OVERLAY_DIGEST_"):
            monkeypatch.delenv(key, raising=False)

    await db_pool.execute(
        "INSERT INTO workflow_schedules ("
        "schedule_id, workflow_name, schedule_kind, schedule_expr, "
        "timezone, catchup_policy, input_json, enabled, next_run_at"
        ") VALUES ($1, 'removed_private_digest', 'cron', '45 7 * * *', "
        "'America/Los_Angeles', 'skip', '{}'::jsonb, TRUE, NOW())",
        "removed_private_digest",
    )

    await sync_registered_workflow_schedules(db_pool)

    enabled = await db_pool.fetchval(
        "SELECT enabled FROM workflow_schedules WHERE schedule_id = $1",
        "removed_private_digest",
    )
    assert enabled is False


@pytest.mark.asyncio
async def test_no_delivery_workflow_is_scheduled(db_pool, monkeypatch, tmp_path):
    from api.workflow_engine import (
        discover_workflow_handlers,
        sync_registered_workflow_schedules,
    )

    wf_file = tmp_path / "db_only.py"
    wf_file.write_text(
        "WORKFLOW_NAME = 'db_only'\n"
        "SCHEDULE = {'interval_seconds': 300, 'no_delivery': True}\n"
        "async def handler(inp, ctx):\n"
        "    return {'status': 'ok'}\n"
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))
    discover_workflow_handlers()

    await sync_registered_workflow_schedules(db_pool)

    row = await db_pool.fetchrow(
        "SELECT enabled, interval_seconds FROM workflow_schedules "
        "WHERE schedule_id = 'db_only'",
    )
    assert row is not None
    assert row["enabled"] is True
    assert row["interval_seconds"] == 300


@pytest.mark.asyncio
async def test_handler_discovery(db_pool, monkeypatch, tmp_path):
    from api.workflow_engine import (
        discover_workflow_handlers,
        get_workflow_handler,
    )

    overlay_workflow = tmp_path / "sample_overlay_digest.py"
    overlay_workflow.write_text(
        "WORKFLOW_NAME = 'sample_overlay_digest'\n"
        "PROMPT = 'Generate the sample overlay digest.'\n"
    )
    monkeypatch.setenv("WORKFLOW_DIRS", str(tmp_path))

    discovered = discover_workflow_handlers()
    assert "agent_turn" in discovered
    assert "slack_thread_turn" in discovered
    assert "sample_overlay_digest" in discovered

    registered = get_workflow_handler("slack_thread_turn")
    assert registered is not None
    assert callable(registered.handler)
    assert registered.input_cls is not None

    overlay_registered = get_workflow_handler("sample_overlay_digest")
    assert overlay_registered is not None
    assert callable(overlay_registered.handler)

    unknown = get_workflow_handler("nonexistent_workflow")
    assert unknown is None


@pytest.mark.asyncio
async def test_child_workflow_lineage_and_waiting_state(db_pool):
    from api.workflow_engine import (
        _RegisteredHandler,
        _WORKFLOW_HANDLERS,
        _claim_run,
        _run_handler,
        create_workflow_run,
        get_workflow_run,
        list_workflow_runs,
    )

    async def child_handler(inp, ctx):
        return {"value": inp["value"], "thread": inp.get("thread_key")}

    async def parent_handler(inp, ctx):
        child = await ctx.run_workflow(
            "child-review",
            workflow_name="test_child_workflow",
            run_input={"value": inp["value"]},
        )
        return {
            "child_run_id": child["run_id"],
            "child_status": child["status"],
            "child_output": child["output_json"],
        }

    with patch.dict(
        _WORKFLOW_HANDLERS,
        {
            "test_parent_workflow": _RegisteredHandler(
                handler=parent_handler,
                input_cls=None,
                source_path="tests:test_parent_workflow",
                version="test-parent-v1",
            ),
            "test_child_workflow": _RegisteredHandler(
                handler=child_handler,
                input_cls=None,
                source_path="tests:test_child_workflow",
                version="test-child-v1",
            ),
        },
        clear=False,
    ):
        parent = await create_workflow_run(
            db_pool,
            workflow_name="test_parent_workflow",
            run_input={"value": 7},
            trigger_key=None,
            eager_start=False,
        )

        first = await _claim_run(db_pool)
        assert first is not None
        assert first["run_id"] == parent["run_id"]
        await _run_handler(db_pool, first)

        waiting_parent = await get_workflow_run(db_pool, parent["run_id"])
        assert waiting_parent is not None
        assert waiting_parent["status"] == "waiting"
        assert waiting_parent["workflow_version"] == "test-parent-v1"
        assert waiting_parent["root_run_id"] == parent["run_id"]
        assert waiting_parent["child_runs_count"] == 1
        assert waiting_parent["latest_step_kind"] == "child_workflow_wait"
        assert waiting_parent["waiting_on"] is not None
        assert waiting_parent["waiting_on"]["type"] == "workflow"
        assert waiting_parent["waiting_on"]["workflow_name"] == "test_child_workflow"
        assert waiting_parent["waiting_on"]["deadline"] is None

        children = await list_workflow_runs(
            db_pool,
            parent_run_id=parent["run_id"],
        )
        assert len(children["items"]) == 1
        child = children["items"][0]
        assert child["parent_run_id"] == parent["run_id"]
        assert child["root_run_id"] == parent["run_id"]
        assert child["workflow_version"] == "test-child-v1"

        second = await _claim_run(db_pool)
        assert second is not None
        assert second["run_id"] == child["run_id"]
        await _run_handler(db_pool, second)

        third = await _claim_run(db_pool)
        assert third is not None
        assert third["run_id"] == parent["run_id"]
        await _run_handler(db_pool, third)

        completed_parent = await get_workflow_run(db_pool, parent["run_id"])
        assert completed_parent is not None
        assert completed_parent["status"] == "completed"
        assert completed_parent["waiting_on"] is None
        assert completed_parent["output_json"] == {
            "child_run_id": child["run_id"],
            "child_status": "completed",
            "child_output": {"value": 7, "thread": None},
        }


@pytest.mark.asyncio
async def test_post_to_slack_raises_on_tool_error(db_pool):
    """Slack delivery errors returned by the tool must fail the workflow step."""
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    tool_manager = AsyncMock()
    tool_manager.call_tool.return_value = json.dumps({
        "error": "Channel 'investing' not found or bot not a member",
        "tool": "slack",
        "method": "send_message",
    })

    with patch("api.app.get_tool_manager", return_value=tool_manager):
        with pytest.raises(RuntimeError, match="not found or bot not a member"):
            await ctx.post_to_slack("investing", "hello")

    checkpoint = await db_pool.fetchrow(
        "SELECT checkpoint_name FROM workflow_checkpoints WHERE run_id = $1",
        run_id,
    )
    assert checkpoint is None


@pytest.mark.asyncio
async def test_step_retry_with_backoff(db_pool):
    """ctx.step() retries on failure with configured policy."""
    from api.workflow_engine import RetryPolicy, WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    call_count = 0

    async def flaky_fn():
        nonlocal call_count
        call_count += 1
        if call_count < 3:
            raise RuntimeError("transient failure")
        return {"ok": True}

    result = await ctx.step(
        "flaky",
        flaky_fn,
        retry=RetryPolicy(limit=4, delay=dt.timedelta(milliseconds=1)),
    )
    assert result == {"ok": True}
    assert call_count == 3


@pytest.mark.asyncio
async def test_step_non_retryable_error_skips_retries(db_pool):
    """NonRetryableError propagates immediately without retrying."""
    from api.workflow_engine import NonRetryableError, RetryPolicy, WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    call_count = 0

    async def permanent_failure():
        nonlocal call_count
        call_count += 1
        raise NonRetryableError("bad input")

    with pytest.raises(NonRetryableError, match="bad input"):
        await ctx.step(
            "permanent",
            permanent_failure,
            retry=RetryPolicy(limit=5),
        )
    assert call_count == 1


@pytest.mark.asyncio
async def test_step_timeout(db_pool):
    """ctx.step() raises TimeoutError when fn exceeds timeout."""
    from api.workflow_engine import WorkflowContext
    import asyncio as _asyncio

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    async def slow_fn():
        await _asyncio.sleep(10)
        return {"done": True}

    with pytest.raises(TimeoutError):
        await ctx.step(
            "slow",
            slow_fn,
            timeout=dt.timedelta(milliseconds=50),
        )


@pytest.mark.asyncio
async def test_sleep_until(db_pool):
    """ctx.sleep_until() falls through when wake time is in the past."""
    from api.workflow_engine import WorkflowContext, SuspendWorkflow
    import datetime as _dt

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    # Future time → should suspend
    future = _dt.datetime.now(_dt.timezone.utc) + _dt.timedelta(hours=1)
    with pytest.raises(SuspendWorkflow):
        await ctx.sleep_until("wait_future", future)

    # Past time in checkpoint → should fall through
    past = _dt.datetime.now(_dt.timezone.utc) - _dt.timedelta(hours=1)
    ctx2 = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={"wait_past": past.isoformat()},
        lease_s=30.0,
        worker_id="w1",
    )
    await ctx2.sleep_until("wait_past", past)


@pytest.mark.asyncio
async def test_replay_safe_logging(db_pool):
    """ctx.log() is suppressed during replay, active after first cache miss."""
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    # Fresh context (no checkpoints) → _in_replay is False → log emits
    ctx_fresh = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    assert ctx_fresh._in_replay is False

    # Context with checkpoints → _in_replay is True → log suppressed
    ctx_replay = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={"step1": {"cached": True}},
        lease_s=30.0,
        worker_id="w1",
    )
    assert ctx_replay._in_replay is True


@pytest.mark.asyncio
async def test_wait_for_event_and_send_event(db_pool):
    """wait_for_event suspends, send_workflow_event wakes and delivers."""
    from api.workflow_engine import (
        SuspendWorkflow,
        WorkflowContext,
        send_workflow_event,
    )

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )

    # First call: event doesn't exist yet → should suspend
    with pytest.raises(SuspendWorkflow):
        await ctx.wait_for_event(
            "approval",
            event_type="deploy.approval",
            correlation_id="deploy-123",
        )

    # Verify wait marker checkpoint was written
    cp = await db_pool.fetchrow(
        "SELECT state, step_kind FROM workflow_checkpoints "
        "WHERE run_id = $1 AND checkpoint_name = 'approval'",
        run_id,
    )
    assert cp is not None
    assert cp["step_kind"] == "event_wait"
    state = json.loads(cp["state"]) if isinstance(cp["state"], str) else cp["state"]
    assert state["_waiting"] is True

    # Mark run as waiting so send_workflow_event can find it
    await db_pool.execute(
        "UPDATE workflow_runs SET status = 'waiting' WHERE run_id = $1",
        run_id,
    )

    # Send the event
    result = await send_workflow_event(
        db_pool,
        event_type="deploy.approval",
        correlation_id="deploy-123",
        payload={"approved": True, "by": "alice"},
    )
    assert result["ok"] is True
    assert result["runs_woken"] == 1

    # Now replay: ctx should find the event and return it
    checkpoints = {"approval": state}  # still the wait marker
    ctx2 = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints=checkpoints,
        lease_s=30.0,
        worker_id="w1",
    )
    payload = await ctx2.wait_for_event(
        "approval",
        event_type="deploy.approval",
        correlation_id="deploy-123",
    )
    assert payload["approved"] is True
    assert payload["by"] == "alice"


@pytest.mark.asyncio
async def test_wait_for_event_timeout(db_pool):
    """wait_for_event raises TimeoutError when deadline passes."""
    from api.workflow_engine import WorkflowContext
    import datetime as _dt

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )

    # Simulate a wait marker with a deadline that already passed
    past_deadline = (
        _dt.datetime.now(_dt.timezone.utc) - _dt.timedelta(hours=1)
    ).isoformat()
    checkpoints = {
        "approval": {
            "_waiting": True,
            "event_type": "deploy.approval",
            "correlation_id": "deploy-456",
            "deadline": past_deadline,
        },
    }
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints=checkpoints,
        lease_s=30.0,
        worker_id="w1",
    )

    with pytest.raises(TimeoutError):
        await ctx.wait_for_event(
            "approval",
            event_type="deploy.approval",
            correlation_id="deploy-456",
        )


@pytest.mark.asyncio
async def test_wait_for_event_returns_payload_after_deadline_if_event_exists(db_pool):
    from api.workflow_engine import WorkflowContext

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test', 'test-v1', 'hash', $1, 'running', '{}'::jsonb, 'w1')",
        run_id,
    )
    await db_pool.execute(
        "INSERT INTO workflow_events (event_type, correlation_id, payload) "
        "VALUES ('deploy.approval', 'deploy-789', $1::jsonb)",
        json.dumps({"approved": True, "by": "alice"}),
    )

    past_deadline = (
        dt.datetime.now(dt.timezone.utc) - dt.timedelta(minutes=1)
    ).isoformat()
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={
            "approval": {
                "_waiting": True,
                "event_type": "deploy.approval",
                "correlation_id": "deploy-789",
                "deadline": past_deadline,
            },
        },
        lease_s=30.0,
        worker_id="w1",
    )

    payload = await ctx.wait_for_event(
        "approval",
        event_type="deploy.approval",
        correlation_id="deploy-789",
    )

    assert payload == {"approved": True, "by": "alice"}


@pytest.mark.asyncio
async def test_wait_for_workflow_returns_completed_child_after_deadline(db_pool):
    from api.workflow_engine import WorkflowContext

    parent_run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    child_run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, worker_id"
        ") VALUES ($1, 'test_parent', 'test-v1', 'hash-parent', $1, 'running', '{}'::jsonb, 'w1')",
        parent_run_id,
    )
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
        "status, input_json, output_json, completed_at"
        ") VALUES ($1, 'test_child', 'test-v1', 'hash-child', $1, 'completed', '{}'::jsonb, $2::jsonb, NOW())",
        child_run_id,
        json.dumps({"ok": True}),
    )

    past_deadline = (
        dt.datetime.now(dt.timezone.utc) - dt.timedelta(minutes=1)
    ).isoformat()
    ctx = WorkflowContext(
        pool=db_pool,
        run_id=parent_run_id,
        checkpoints={
            "child.wait": {
                "_waiting": True,
                "child_run_id": child_run_id,
                "workflow_name": "test_child",
                "deadline": past_deadline,
            },
        },
        lease_s=30.0,
        worker_id="w1",
    )

    child = await ctx.wait_for_workflow("child.wait", run_id=child_run_id)

    assert child["run_id"] == child_run_id
    assert child["status"] == "completed"
    assert child["output_json"] == {"ok": True}


async def _insert_session_and_execution(
    db_pool, *, thread_key: str, status: str, terminal_reason: str | None
) -> None:
    await db_pool.execute(
        "INSERT INTO sandbox_sessions ("
        "thread_key, sandbox_id, harness, engine, state, agent_thread_id"
        ") VALUES ($1, 'sbx-old', 'codex', 'codex', 'stopped', 'broken-agent-thread')",
        thread_key,
    )
    await db_pool.execute(
        "INSERT INTO agent_execution_requests ("
        "execution_id, thread_key, assignment_generation, execute_id, request_hash, "
        "status, terminal_reason"
        ") VALUES ($1, $2, 1, $3, 'hash', $4, $5)",
        f"exe_{uuid.uuid4().hex[:16]}",
        thread_key,
        f"exec:{uuid.uuid4().hex[:8]}",
        status,
        terminal_reason,
    )


@pytest.mark.asyncio
async def test_recovery_turn_resets_session_after_permanent_failure(db_pool):
    """An explicit retry after a failed_permanent execution must clear the
    stale harness session (e.g. a codex thread whose rollout file died with
    the old sandbox) so the turn runs on a fresh runtime."""
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    await _insert_session_and_execution(
        db_pool, thread_key=thread_key, status="failed_permanent",
        terminal_reason="harness_error",
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})
    release_assignment_mock = AsyncMock(return_value={"ok": True, "released": True})

    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch("api.runtime_control.release_assignment", new=release_assignment_mock),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "retry"}],
                message_id="slack:current",
            ),
            ctx,
        )

    release_assignment_mock.assert_awaited_once_with(
        db_pool,
        thread_key=thread_key,
        release_id="recovery-reset:slack:current",
        cancel_inflight=True,
        stop_runtime_background=True,
    )
    row = await db_pool.fetchrow(
        "SELECT state, agent_thread_id FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    assert row is not None
    assert row["state"] == "stopped"
    assert row["agent_thread_id"] is None


@pytest.mark.asyncio
async def test_recovery_turn_keeps_session_after_completed_execution(db_pool):
    """Retry/continue on a healthy thread must not discard the live harness
    session and its context."""
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    await _insert_session_and_execution(
        db_pool, thread_key=thread_key, status="completed",
        terminal_reason="completed",
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})
    release_assignment_mock = AsyncMock(return_value={"ok": True, "released": True})

    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch("api.runtime_control.release_assignment", new=release_assignment_mock),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "retry"}],
                message_id="slack:current",
            ),
            ctx,
        )

    release_assignment_mock.assert_not_awaited()
    row = await db_pool.fetchrow(
        "SELECT agent_thread_id FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    assert row["agent_thread_id"] == "broken-agent-thread"


@pytest.mark.asyncio
async def test_non_recovery_turn_keeps_session_after_permanent_failure(db_pool):
    """Ordinary messages never force a reset; in-sandbox recovery (resume
    fallback) handles those without discarding state."""
    from api.workflow_engine import WorkflowContext
    from api.workflows.slack_thread_turn import Input, handler

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    thread_key = f"slack:C-test:{uuid.uuid4().hex}"
    await _insert_session_and_execution(
        db_pool, thread_key=thread_key, status="failed_permanent",
        terminal_reason="harness_error",
    )

    ctx = WorkflowContext(
        pool=db_pool,
        run_id=run_id,
        checkpoints={},
        lease_s=30.0,
        worker_id="w1",
    )
    do_agent_turn_mock = AsyncMock(return_value={"ok": True, "execution_id": "exe-1"})
    release_assignment_mock = AsyncMock(return_value={"ok": True, "released": True})

    with (
        patch("api.workflow_engine.do_agent_turn", new=do_agent_turn_mock),
        patch("api.runtime_control.release_assignment", new=release_assignment_mock),
    ):
        await handler(
            Input(
                thread_key=thread_key,
                parts=[{"type": "text", "text": "Summarize this thread"}],
                message_id="slack:current",
            ),
            ctx,
        )

    release_assignment_mock.assert_not_awaited()
    row = await db_pool.fetchrow(
        "SELECT agent_thread_id FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    assert row["agent_thread_id"] == "broken-agent-thread"
