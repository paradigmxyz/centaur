"""Unit tests for restart-safe in-flight turn replay behavior."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from api.sandbox.base import SandboxSession  # noqa: E402


def test_coerce_json_object_handles_jsonb_text() -> None:
    from api.agent import _coerce_json_object

    payload = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": "hello"}],
        },
    }

    assert _coerce_json_object(payload) == payload
    assert _coerce_json_object(json.dumps(payload)) == payload
    assert _coerce_json_object(json.dumps(json.dumps(payload))) == payload
    assert _coerce_json_object("not-json") is None


@pytest.mark.asyncio
async def test_replay_inflight_turn_noop_when_no_turn() -> None:
    session = SandboxSession(
        sandbox_id="sbx-1",
        thread_key="test:thread-1",
        harness="amp",
        engine="amp",
    )

    with patch(
        "api.agent._db_get_inflight_turn", new_callable=AsyncMock, return_value=None
    ):
        from api.agent import replay_inflight_turn

        result = await replay_inflight_turn(session)

    assert result == {"ok": True, "replayed": False}


@pytest.mark.asyncio
async def test_replay_inflight_turn_writes_payload() -> None:
    session = SandboxSession(
        sandbox_id="sbx-2",
        thread_key="test:thread-2",
        harness="amp",
        engine="amp",
    )
    turn_input = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": "hello"}],
        },
    }

    backend = AsyncMock()
    backend.refresh_token_by_id = AsyncMock()
    backend.attach = AsyncMock()
    backend.write_stdin = AsyncMock()

    with (
        patch(
            "api.agent._db_get_inflight_turn",
            new_callable=AsyncMock,
            return_value=("turn-abc", turn_input, 1),
        ),
        patch(
            "api.agent._db_set_inflight_turn", new_callable=AsyncMock
        ) as set_inflight,
        patch("api.agent._db_update_state", new_callable=AsyncMock),
        patch("api.agent.get_backend", return_value=backend),
        patch("api.agent.mint_sandbox_token", return_value="sbx-token"),
    ):
        from api.agent import replay_inflight_turn

        result = await replay_inflight_turn(session)

    assert result["ok"] is True
    assert result["replayed"] is True
    assert result["durable_turn_id"] == "turn-abc"
    backend.write_stdin.assert_awaited_once_with(session, turn_input)
    set_inflight.assert_awaited_once_with(
        session.thread_key,
        "turn-abc",
        turn_input,
        attempts=2,
    )


@pytest.mark.asyncio
async def test_replay_inflight_turn_recovers_broken_stdin() -> None:
    session = SandboxSession(
        sandbox_id="sbx-2",
        thread_key="test:thread-2",
        harness="amp",
        engine="amp",
    )
    turn_input = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": "hello"}],
        },
    }

    backend = AsyncMock()
    backend.refresh_token_by_id = AsyncMock()
    backend.attach = AsyncMock()
    backend.status = AsyncMock(return_value="running")
    backend.reattach_stdin = AsyncMock()
    backend.write_stdin = AsyncMock(side_effect=[BrokenPipeError("closed"), None])

    with (
        patch(
            "api.agent._db_get_inflight_turn",
            new_callable=AsyncMock,
            return_value=("turn-abc", turn_input, 1),
        ),
        patch("api.agent._db_set_inflight_turn", new_callable=AsyncMock),
        patch("api.agent._db_update_state", new_callable=AsyncMock),
        patch("api.agent.get_backend", return_value=backend),
        patch("api.agent.mint_sandbox_token", return_value="sbx-token"),
    ):
        from api.agent import replay_inflight_turn

        result = await replay_inflight_turn(session)

    assert result["ok"] is True
    backend.reattach_stdin.assert_awaited_once_with(session)
    assert backend.write_stdin.await_count == 2


@pytest.mark.asyncio
async def test_inject_stdin_persists_inflight_turn() -> None:
    session = SandboxSession(
        sandbox_id="sbx-3",
        thread_key="test:thread-3",
        harness="amp",
        engine="amp",
    )

    backend = AsyncMock()
    backend.refresh_token_by_id = AsyncMock()
    backend.attach = AsyncMock()
    backend.write_stdin = AsyncMock()

    with (
        patch("api.agent._insert_system_message", new_callable=AsyncMock),
        patch(
            "api.agent._get_last_delivered_id",
            new_callable=AsyncMock,
            return_value=None,
        ),
        patch("api.agent._flush_pending", new_callable=AsyncMock, return_value=[]),
        patch(
            "api.agent._db_set_inflight_turn", new_callable=AsyncMock
        ) as set_inflight,
        patch("api.agent._db_update_state", new_callable=AsyncMock),
        patch("api.agent._advance_cursor", new_callable=AsyncMock),
        patch("api.agent.get_backend", return_value=backend),
        patch("api.agent.mint_sandbox_token", return_value="sbx-token"),
    ):
        from api.agent import inject_stdin

        result = await inject_stdin(session, "hello")

    assert result["ok"] is True
    assert result["injected"] is True
    assert result["durable_turn_id"].startswith("turn-")
    set_inflight.assert_awaited_once()


@pytest.mark.asyncio
async def test_inject_stdin_prepends_current_session_context_after_cursor() -> None:
    session = SandboxSession(
        sandbox_id="sbx-4",
        thread_key="slack:C123:1712345678.000100",
        harness="amp",
        engine="amp",
    )

    backend = AsyncMock()
    backend.refresh_token_by_id = AsyncMock()
    backend.attach = AsyncMock()
    backend.write_stdin = AsyncMock()

    with (
        patch(
            "api.agent._insert_system_message",
            new_callable=AsyncMock,
            return_value=(
                "# Session Context\n\n"
                "## GitHub PR Attribution\n\n"
                "- If you create a GitHub PR for this Slack request, "
                "the PR body MUST contain this standalone line: `Prompted by: @alice`"
            ),
        ),
        patch(
            "api.agent._get_last_delivered_id",
            new_callable=AsyncMock,
            return_value="msg-after-system",
        ),
        patch("api.agent._flush_pending", new_callable=AsyncMock, return_value=[]),
        patch("api.agent._db_set_inflight_turn", new_callable=AsyncMock),
        patch("api.agent._db_update_state", new_callable=AsyncMock),
        patch("api.agent._advance_cursor", new_callable=AsyncMock),
        patch("api.agent.get_backend", return_value=backend),
        patch("api.agent.mint_sandbox_token", return_value="sbx-token"),
    ):
        from api.agent import inject_stdin

        result = await inject_stdin(
            session,
            "please make a PR",
            platform="slack",
            user_id="U123",
        )

    assert result["ok"] is True
    payload = backend.write_stdin.await_args.args[1]
    content = payload["message"]["content"]
    assert "Prompted by: @alice" in content[0]["text"]
    assert content[1] == {"type": "text", "text": "please make a PR"}


@pytest.mark.asyncio
async def test_inject_stdin_deduplicates_queued_session_context() -> None:
    session = SandboxSession(
        sandbox_id="sbx-5",
        thread_key="slack:C123:1712345678.000200",
        harness="amp",
        engine="amp",
    )

    backend = AsyncMock()
    backend.refresh_token_by_id = AsyncMock()
    backend.attach = AsyncMock()
    backend.write_stdin = AsyncMock()

    system_id = f"system-{session.thread_key}-slack"
    with (
        patch(
            "api.agent._insert_system_message",
            new_callable=AsyncMock,
            return_value="fresh session context with Prompted by: @alice",
        ),
        patch(
            "api.agent._get_last_delivered_id",
            new_callable=AsyncMock,
            return_value=None,
        ),
        patch(
            "api.agent._flush_pending",
            new_callable=AsyncMock,
            return_value=[
                {
                    "id": system_id,
                    "role": "system",
                    "parts": [{"type": "text", "text": "stale session context"}],
                    "metadata": {},
                },
                {
                    "id": "msg-user",
                    "role": "user",
                    "parts": [{"type": "text", "text": "make a PR"}],
                    "metadata": {},
                },
            ],
        ),
        patch("api.agent._db_set_inflight_turn", new_callable=AsyncMock),
        patch("api.agent._db_update_state", new_callable=AsyncMock),
        patch("api.agent._advance_cursor", new_callable=AsyncMock),
        patch("api.agent.get_backend", return_value=backend),
        patch("api.agent.mint_sandbox_token", return_value="sbx-token"),
    ):
        from api.agent import inject_stdin

        result = await inject_stdin(session, "", platform="slack", user_id="U123")

    assert result["ok"] is True
    payload = backend.write_stdin.await_args.args[1]
    texts = [part["text"] for part in payload["message"]["content"]]
    assert texts == [
        "fresh session context with Prompted by: @alice",
        "make a PR",
    ]


@pytest.mark.asyncio
async def test_flush_pending_skips_assistant_messages(db_pool) -> None:
    thread_key = "test:thread-flush"
    user_one = "msg-1"
    assistant = "asst-1"
    assistant_history = "asst-history-1"
    user_two = "msg-2"

    await db_pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, metadata, created_at) VALUES "
        '($1, $5, \'user\', \'[{"type":"text","text":"first"}]\'::jsonb, \'{}\'::jsonb, '
        " TIMESTAMPTZ '2026-01-01T00:00:00Z'), "
        '($2, $5, \'assistant\', \'[{"type":"text","text":"reply"}]\'::jsonb, \'{}\'::jsonb, '
        " TIMESTAMPTZ '2026-01-01T00:00:01Z'), "
        '($3, $5, \'assistant\', \'[{"type":"text","text":"imported reply"}]\'::jsonb, '
        '\'{"history_backfill": true}\'::jsonb, TIMESTAMPTZ \'2026-01-01T00:00:01.5Z\'), '
        '($4, $5, \'user\', \'[{"type":"text","text":"second"}]\'::jsonb, \'{}\'::jsonb, '
        " TIMESTAMPTZ '2026-01-01T00:00:02Z')",
        user_one,
        assistant,
        assistant_history,
        user_two,
        thread_key,
    )

    with patch("api.agent._get_pool", return_value=db_pool):
        from api.agent import _flush_pending

        rows = await _flush_pending(thread_key, user_one)

    assert [row["id"] for row in rows] == [assistant_history, user_two]


def test_flushed_history_backfill_marks_imported_assistant_context() -> None:
    from api.agent import _flushed_to_messages

    messages = _flushed_to_messages([
        {
            "id": "asst-history-1",
            "role": "assistant",
            "parts": [{"type": "text", "text": "prior answer"}],
            "metadata": {"history_backfill": True},
        },
    ])

    assert messages == [
        {
            "role": "assistant",
            "parts": [{"type": "text", "text": "prior answer"}],
            "history_backfill": True,
        },
    ]
