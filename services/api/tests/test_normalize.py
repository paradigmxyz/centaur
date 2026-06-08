"""Tests for normalize — canonical event normalization."""

from api.sandbox.normalize import normalize_harness_event


class TestAmpLike:
    def test_assistant_passthrough(self):
        evt = {
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "hello"}]},
        }
        result = normalize_harness_event("amp", evt)
        assert result == [evt]

    def test_user_tool_result(self):
        evt = {
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "t1",
                        "content": "ok",
                        "is_error": False,
                    },
                ]
            },
        }
        result = normalize_harness_event("amp", evt)
        assert len(result) == 1
        assert result[0]["type"] == "tool"
        assert result[0]["content"][0]["tool_use_id"] == "t1"

    def test_result_event(self):
        result = normalize_harness_event("amp", {"type": "result", "result": "done"})
        assert result == [{"type": "result", "text": "done"}]

    def test_error_event(self):
        result = normalize_harness_event("amp", {"type": "error", "error": "oops"})
        assert result == [{"type": "error", "error": "oops"}]

    def test_error_event_nested_message(self):
        result = normalize_harness_event(
            "amp",
            {"type": "error", "error": {"message": "amp exited with code 1"}},
        )
        assert result == [{"type": "error", "error": "amp exited with code 1"}]

    def test_amp_wrapper_restart_notice_is_not_user_visible(self):
        result = normalize_harness_event(
            "amp",
            {
                "type": "error",
                "error": {"message": "amp exited with code 1, restarting (1/5)"},
            },
        )
        assert result == []

    def test_amp_wrapper_give_up_error_is_user_visible(self):
        result = normalize_harness_event(
            "amp",
            {
                "type": "error",
                "error": {"message": "amp crashed 6 times, giving up"},
            },
        )
        assert result == [{"type": "error", "error": "amp crashed 6 times, giving up"}]

    def test_system_init(self):
        result = normalize_harness_event(
            "amp",
            {
                "type": "system",
                "subtype": "init",
                "session_id": "T-abc",
            },
        )
        assert result == [{"type": "system", "subtype": "init", "session_id": "T-abc"}]

    def test_reasoning_passthrough(self):
        evt = {"type": "reasoning", "text": "thinking..."}
        result = normalize_harness_event("amp", evt)
        assert result == [evt]

    def test_subagent_normalize_status(self):
        evt = {
            "type": "subagent",
            "status": "started",
            "subagent_id": "sub1",
            "name": "Worker",
        }
        result = normalize_harness_event("amp", evt)
        assert len(result) == 1
        assert result[0]["status"] == "started"
        assert result[0]["subagent_id"] == "sub1"

    def test_unknown_event_ignored(self):
        assert normalize_harness_event("amp", {"type": "ping"}) == []

    def test_persona_uses_amp_like(self):
        """Persona names (legal, eng) should use amp-like normalizer."""
        result = normalize_harness_event("legal", {"type": "result", "result": "done"})
        assert result == [{"type": "result", "text": "done"}]


class TestCodex:
    def test_thread_started(self):
        result = normalize_harness_event(
            "codex",
            {
                "type": "thread.started",
                "thread_id": "thread-1",
            },
        )
        assert result == [
            {"type": "system", "subtype": "init", "session_id": "thread-1"}
        ]

    def test_item_completed_message(self):
        event = {
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "hello"},
        }
        result = normalize_harness_event(
            "codex",
            event,
        )
        assert result == [event]

    def test_agent_message_delta_passthrough(self):
        event = {
            "type": "item.agentMessage.delta",
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "msg-1",
            "delta": "hello",
        }
        assert normalize_harness_event("codex", event) == [event]

    def test_command_execution_camel_case_item(self):
        event = {
            "type": "item.completed",
            "item": {
                "type": "commandExecution",
                "command": "node hello.js",
                "aggregated_output": "hello\n",
                "exit_code": 0,
                "status": "completed",
            },
        }
        result = normalize_harness_event(
            "codex",
            event,
        )
        assert result == [event]

    def test_turn_failed(self):
        result = normalize_harness_event(
            "codex",
            {
                "type": "turn.failed",
                "error": {"message": "boom"},
            },
        )
        assert result == [{"type": "error", "error": "boom"}]

    def test_error_event_string_error(self):
        result = normalize_harness_event(
            "codex",
            {
                "type": "error",
                "error": "Error running remote compact task",
            },
        )
        assert result == [{"type": "error", "error": "Error running remote compact task"}]


class TestPiMono:
    def test_session(self):
        result = normalize_harness_event(
            "pi-mono",
            {
                "type": "session",
                "id": "sess-1",
            },
        )
        assert result == [{"type": "system", "subtype": "init", "session_id": "sess-1"}]

    def test_tool_execution_start(self):
        result = normalize_harness_event(
            "pi-mono",
            {
                "type": "tool_execution_start",
                "toolName": "Read",
                "toolCallId": "tc1",
                "args": {"path": "/foo"},
            },
        )
        assert len(result) == 1
        assert result[0]["type"] == "assistant"
        content = result[0]["message"]["content"]
        assert content[0]["type"] == "tool_use"
        assert content[0]["name"] == "Read"

    def test_tool_execution_end(self):
        result = normalize_harness_event(
            "pi-mono",
            {
                "type": "tool_execution_end",
                "toolCallId": "tc1",
                "toolName": "Read",
                "result": "file content",
            },
        )
        assert len(result) == 1
        assert result[0]["type"] == "tool"
        assert result[0]["content"][0]["tool_use_id"] == "tc1"


class TestAutoDetect:
    def test_detect_codex(self):
        result = normalize_harness_event(
            "", {"type": "item.started", "item": {"type": "error", "message": "x"}}
        )
        assert result == [{"type": "error", "error": "x"}]

    def test_detect_pi(self):
        result = normalize_harness_event("", {"type": "session", "id": "s1"})
        assert result == [{"type": "system", "subtype": "init", "session_id": "s1"}]

    def test_detect_amp_fallback(self):
        result = normalize_harness_event("", {"type": "result", "result": "ok"})
        assert result == [{"type": "result", "text": "ok"}]
