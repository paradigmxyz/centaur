"""Tests for harness_protocol — pure function tests, no infra needed."""

from api.sandbox.harness_protocol import (
    build_user_input,
    extract_result,
    extract_thread_id,
    is_turn_done,
    messages_to_content_blocks,
)


class TestIsTurnDone:
    # amp / claude-code ---------------------------------------------------

    def test_amp_result_event(self):
        assert is_turn_done("amp", {"type": "result", "result": "done"}) is True

    def test_amp_assistant_end_turn(self):
        event = {
            "type": "assistant",
            "message": {"stop_reason": "end_turn"},
        }
        assert is_turn_done("amp", event) is True

    def test_amp_assistant_tool_use_end_turn_not_done(self):
        event = {
            "type": "assistant",
            "message": {
                "stop_reason": "end_turn",
                "content": [{"type": "tool_use", "id": "toolu_123"}],
            },
        }
        assert is_turn_done("amp", event) is False

    def test_amp_assistant_not_end_turn(self):
        event = {
            "type": "assistant",
            "message": {"stop_reason": "tool_use"},
        }
        assert is_turn_done("amp", event) is False

    def test_amp_subagent_end_turn_ignored(self):
        """Subagent end_turn (parent_tool_use_id set) must NOT signal turn done."""
        event = {
            "type": "assistant",
            "parent_tool_use_id": "toolu_abc123",
            "message": {"stop_reason": "end_turn"},
        }
        assert is_turn_done("amp", event) is False

    def test_amp_main_agent_end_turn_no_parent(self):
        """Main agent end_turn (parent_tool_use_id is None/absent) signals done."""
        event = {
            "type": "assistant",
            "parent_tool_use_id": None,
            "message": {"stop_reason": "end_turn"},
        }
        assert is_turn_done("amp", event) is True

    def test_amp_other_event(self):
        assert is_turn_done("amp", {"type": "system"}) is False

    def test_amp_wrapper_restart_error_not_done(self):
        event = {
            "type": "error",
            "error": {"message": "amp exited with code 1, restarting (1/5)"},
        }
        assert is_turn_done("amp", event) is False

    def test_amp_wrapper_give_up_error_done(self):
        event = {
            "type": "error",
            "error": {"message": "amp crashed 6 times, giving up"},
        }
        assert is_turn_done("amp", event) is True

    def test_claude_code_result_event(self):
        assert is_turn_done("claude-code", {"type": "result"}) is True

    # codex ---------------------------------------------------------------

    def test_codex_turn_completed(self):
        assert is_turn_done("codex", {"type": "turn.completed"}) is True

    def test_codex_turn_failed(self):
        assert is_turn_done("codex", {"type": "turn.failed"}) is True

    def test_codex_other_event(self):
        assert is_turn_done("codex", {"type": "item.completed"}) is False

    # pi-mono -------------------------------------------------------------

    def test_pi_mono_agent_end(self):
        assert is_turn_done("pi-mono", {"type": "agent_end"}) is True

    def test_pi_mono_other_event(self):
        assert is_turn_done("pi-mono", {"type": "message_end"}) is False


class TestExtractResult:
    # amp / claude-code ---------------------------------------------------

    def test_turn_done_result_event(self):
        assert extract_result("amp", {"type": "turn.done", "result": "hello"}) == "hello"

    def test_turn_done_dict_result_event(self):
        assert (
            extract_result("claude-code", {"type": "turn.done", "result": {"text": "hello"}})
            == "hello"
        )

    def test_amp_result_event(self):
        assert extract_result("amp", {"type": "result", "result": "hello"}) == "hello"

    def test_claude_code_result_event_with_text_field(self):
        event = {"type": "result", "text": "final synthesis"}

        assert extract_result("claude-code", event) == "final synthesis"

    def test_amp_error_result_uses_error_message(self):
        event = {
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": True,
            "error": "Timed out while reconnecting. Please retry after reconnecting.",
        }
        assert (
            extract_result("amp", event)
            == "Timed out while reconnecting. Please retry after reconnecting."
        )

    def test_amp_assistant_text(self):
        event = {
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": "answer"}],
            },
        }
        assert extract_result("amp", event) == "answer"

    def test_amp_assistant_multiple_texts(self):
        event = {
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "first"},
                    {"type": "tool_use", "id": "t1"},
                    {"type": "text", "text": "second"},
                ],
            },
        }
        assert extract_result("amp", event) == "second"

    def test_amp_other_event_returns_none(self):
        assert extract_result("amp", {"type": "system"}) is None

    # codex ---------------------------------------------------------------

    def test_codex_item_completed(self):
        event = {
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "codex says"},
        }
        assert extract_result("codex", event) == "codex says"

    def test_codex_item_completed_camel_case(self):
        event = {
            "type": "item.completed",
            "item": {"type": "agentMessage", "text": "codex says"},
        }
        assert extract_result("codex", event) == "codex says"

    def test_codex_non_agent_message(self):
        event = {
            "type": "item.completed",
            "item": {"type": "tool_call", "text": "ignored"},
        }
        assert extract_result("codex", event) is None

    # pi-mono -------------------------------------------------------------

    def test_pi_mono_message_end(self):
        event = {
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{"text": "pi answer"}],
            },
        }
        assert extract_result("pi-mono", event) == "pi answer"


class TestExtractThreadId:
    def test_amp_system_init(self):
        event = {"type": "system", "subtype": "init", "session_id": "S-123"}
        assert extract_thread_id("amp", event) == "S-123"

    def test_amp_system_init_empty_session_id(self):
        event = {"type": "system", "subtype": "init", "session_id": ""}
        assert extract_thread_id("amp", event) is None

    def test_amp_assistant_session_id(self):
        event = {
            "type": "assistant",
            "session_id": "T-assistant",
            "message": {"stop_reason": "end_turn", "content": []},
        }
        assert extract_thread_id("amp", event) == "T-assistant"

    def test_codex_thread_started(self):
        event = {"type": "thread.started", "thread_id": "T-abc"}
        assert extract_thread_id("codex", event) == "T-abc"

    def test_pi_mono_session(self):
        event = {"type": "session", "id": "sess-42"}
        assert extract_thread_id("pi-mono", event) == "sess-42"

    def test_unrelated_event_returns_none(self):
        assert extract_thread_id("amp", {"type": "assistant"}) is None
        assert extract_thread_id("codex", {"type": "item.completed"}) is None
        assert extract_thread_id("pi-mono", {"type": "message_end"}) is None


class TestBuildUserInput:
    def test_simple_text(self):
        blocks = [{"type": "text", "text": "hi"}]
        result = build_user_input(blocks)
        assert result == {
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hi"}],
            },
        }

    def test_multiple_blocks(self):
        blocks = [
            {"type": "text", "text": "look at this"},
            {"type": "image", "source": {"type": "base64", "data": "abc"}},
        ]
        result = build_user_input(blocks)
        assert result["message"]["content"] == blocks
        assert result["type"] == "user"
        assert result["message"]["role"] == "user"

    def test_steer_flag(self):
        blocks = [{"type": "text", "text": "stop and do this"}]
        result = build_user_input(blocks, steer=True)
        assert result["steer"] is True
        assert result["type"] == "user"
        assert result["message"]["content"] == blocks

    def test_steer_false_omits_key(self):
        blocks = [{"type": "text", "text": "hi"}]
        result = build_user_input(blocks, steer=False)
        assert "steer" not in result

    def test_steer_default_omits_key(self):
        blocks = [{"type": "text", "text": "hi"}]
        result = build_user_input(blocks)
        assert "steer" not in result

    def test_thread_key_is_included_when_provided(self):
        blocks = [{"type": "text", "text": "hi"}]
        result = build_user_input(blocks, thread_key="slack:C123:1700000000.000100")
        assert result["thread_key"] == "slack:C123:1700000000.000100"

    def test_trace_id_is_included_when_provided(self):
        blocks = [{"type": "text", "text": "hi"}]
        result = build_user_input(
            blocks, trace_id="00000000-0000-0000-0000-000000000123"
        )
        assert result["trace_id"] == "00000000-0000-0000-0000-000000000123"

    def test_traceparent_is_included_when_provided(self):
        blocks = [{"type": "text", "text": "hi"}]
        traceparent = "00-00000000000040008000000000000123-1111111122223333-01"
        result = build_user_input(blocks, traceparent=traceparent)
        assert result["traceparent"] == traceparent


class TestMessagesToContentBlocks:
    def test_simple_text_message(self):
        msgs = [{"parts": [{"type": "text", "text": "hello"}]}]
        assert messages_to_content_blocks(msgs) == [
            {"type": "text", "text": "hello"},
        ]

    def test_user_attribution(self):
        msgs = [
            {
                "user_id": "U999",
                "parts": [{"type": "text", "text": "hello"}],
            }
        ]
        assert messages_to_content_blocks(msgs) == [
            {"type": "text", "text": "<@U999>: hello"},
        ]

    def test_multiple_messages(self):
        msgs = [
            {
                "user_id": "U1",
                "parts": [{"type": "text", "text": "a"}],
            },
            {
                "user_id": "U2",
                "parts": [{"type": "text", "text": "b"}],
            },
        ]
        result = messages_to_content_blocks(msgs)
        assert result == [
            {"type": "text", "text": "<@U1>: a"},
            {"type": "text", "text": "<@U2>: b"},
        ]

    def test_attachment_ref_translation(self):
        msgs = [
            {
                "parts": [
                    {
                        "type": "attachment_ref",
                        "id": "att-1",
                        "name": "report.pdf",
                        "mime_type": "application/pdf",
                    }
                ],
            }
        ]
        result = messages_to_content_blocks(msgs)
        assert len(result) == 1
        assert result[0]["type"] == "text"
        assert "report.pdf" in result[0]["text"]
        assert "application/pdf" in result[0]["text"]
        assert "att-1" in result[0]["text"]
        assert "/agent/attachments/att-1/download" in result[0]["text"]

    def test_mixed_text_and_attachment_ref(self):
        msgs = [
            {
                "user_id": "U5",
                "parts": [
                    {"type": "text", "text": "check this"},
                    {
                        "type": "attachment_ref",
                        "id": "att-2",
                        "name": "img.png",
                        "mime_type": "image/png",
                    },
                    {"type": "text", "text": "and this"},
                ],
            }
        ]
        result = messages_to_content_blocks(msgs)
        assert len(result) == 3
        # First text gets user attribution
        assert result[0] == {"type": "text", "text": "<@U5>: check this"}
        # Attachment ref translated to text
        assert result[1]["type"] == "text"
        assert "img.png" in result[1]["text"]
        assert "att-2" in result[1]["text"]
        # Second text is plain (attribution already done)
        assert result[2] == {"type": "text", "text": "and this"}

    def test_assistant_role_prefixed(self):
        msgs = [
            {
                "role": "user",
                "user_id": "U1",
                "parts": [{"type": "text", "text": "what is 2+2?"}],
            },
            {
                "role": "assistant",
                "parts": [{"type": "text", "text": "The answer is 4."}],
            },
            {
                "role": "user",
                "user_id": "U1",
                "parts": [{"type": "text", "text": "thanks"}],
            },
        ]
        result = messages_to_content_blocks(msgs)
        assert result == [
            {"type": "text", "text": "<@U1>: what is 2+2?"},
            {"type": "text", "text": "[Your previous response]: The answer is 4."},
            {"type": "text", "text": "<@U1>: thanks"},
        ]

    def test_assistant_role_no_user_attribution(self):
        """Assistant messages should never get user_id attribution even if present."""
        msgs = [
            {
                "role": "assistant",
                "user_id": "U1",
                "parts": [{"type": "text", "text": "I said this"}],
            },
        ]
        result = messages_to_content_blocks(msgs)
        assert result == [
            {"type": "text", "text": "[Your previous response]: I said this"},
        ]

    def test_assistant_history_backfill_uses_centaur_label(self):
        """Imported Slack assistant history may belong to another persona/session."""
        msgs = [
            {
                "role": "assistant",
                "history_backfill": True,
                "parts": [{"type": "text", "text": "Prior default-session answer"}],
            },
        ]
        result = messages_to_content_blocks(msgs)
        assert result == [
            {
                "type": "text",
                "text": "[Previous Centaur response]: Prior default-session answer",
            },
        ]

    def test_assistant_non_text_parts_passthrough(self):
        """Non-text parts in assistant messages pass through unchanged."""
        msgs = [
            {
                "role": "assistant",
                "parts": [
                    {"type": "text", "text": "here's an image"},
                    {"type": "image", "source": {"type": "base64", "data": "abc"}},
                ],
            },
        ]
        result = messages_to_content_blocks(msgs)
        assert result == [
            {"type": "text", "text": "[Your previous response]: here's an image"},
            {"type": "image", "source": {"type": "base64", "data": "abc"}},
        ]

    def test_no_user_id(self):
        msgs = [
            {
                "parts": [
                    {"type": "text", "text": "first"},
                    {"type": "text", "text": "second"},
                ],
            }
        ]
        result = messages_to_content_blocks(msgs)
        assert result == [
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
        ]
