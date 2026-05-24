"""Tests for hermes-app-wrapper — command selection + AIAgent event translation.

The wrapper drives Hermes' native ``AIAgent`` in process and imports it lazily,
so the module loads (and its pure helpers + ``TurnEmitter`` callback→event
translation are exercised) without the sandbox-only ``hermes-agent`` package.
"""

from __future__ import annotations

import importlib.util
import io
import json
import sys
from pathlib import Path
from types import ModuleType

from api.sandbox.config import build_harness_cmd

WRAPPER_PY = Path(__file__).resolve().parents[2] / "sandbox" / "hermes-app-wrapper.py"


def _load_wrapper() -> ModuleType:
    spec = importlib.util.spec_from_file_location("hermes_app_wrapper", WRAPPER_PY)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _capture(fn) -> list[dict]:
    """Run ``fn`` while capturing emitted NDJSON lines (emit falls back to stdout)."""
    buf = io.StringIO()
    real = sys.stdout
    sys.stdout = buf
    try:
        fn()
    finally:
        sys.stdout = real
    return [json.loads(line) for line in buf.getvalue().splitlines() if line]


# ── command selection ─────────────────────────────────────────────────────────


def test_build_harness_cmd_selects_hermes_wrapper():
    assert build_harness_cmd("hermes") == ["hermes-app-wrapper"]


def test_module_loads_without_hermes_installed():
    wrapper = _load_wrapper()
    assert hasattr(wrapper, "TurnEmitter")
    assert "run_agent" not in sys.modules


# ── pure helpers ───────────────────────────────────────────────────────────────


def test_prompt_text_joins_blocks():
    wrapper = _load_wrapper()
    turn = {
        "message": {
            "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]
        }
    }
    assert wrapper._prompt_text(turn) == "a\nb"


def test_prompt_text_empty_defaults_to_continue():
    wrapper = _load_wrapper()
    assert wrapper._prompt_text({}) == "continue"


def test_prompt_text_image_placeholder():
    wrapper = _load_wrapper()
    assert "image attachment" in wrapper._prompt_text(
        {"message": {"content": [{"type": "image"}]}}
    )


def test_stringify_passthrough_and_json():
    wrapper = _load_wrapper()
    assert wrapper._stringify("plain") == "plain"
    assert wrapper._stringify(None) == ""
    assert wrapper._stringify({"x": 1}) == json.dumps({"x": 1})


def test_prompt_with_transcript_empty_returns_current_prompt():
    wrapper = _load_wrapper()
    assert wrapper._prompt_with_transcript("current", []) == "current"


def test_prompt_with_transcript_includes_prior_turns():
    wrapper = _load_wrapper()
    prompt = wrapper._prompt_with_transcript(
        "What nonce did I give you?",
        [("Remember nonce HERMES_NONCE_7429.", "SAVED")],
    )

    assert "Previous conversation in this Hermes runtime" in prompt
    assert "User:\nRemember nonce HERMES_NONCE_7429." in prompt
    assert "Assistant:\nSAVED" in prompt
    assert "Current user message:\nWhat nonce did I give you?" in prompt


def test_plan_entries_from_todo_with_trailing_hint():
    wrapper = _load_wrapper()
    result = '{"todos":[{"content":"a","status":"in_progress"},{"content":"b","status":"cancelled"}]} (hint)'
    entries = wrapper._plan_entries_from_todo(result)
    assert entries == [
        {"content": "a", "priority": "medium", "status": "in_progress"},
        {"content": "[cancelled] b", "priority": "medium", "status": "completed"},
    ]


def test_plan_entries_from_todo_invalid_returns_none():
    wrapper = _load_wrapper()
    assert wrapper._plan_entries_from_todo("not json") is None
    assert wrapper._plan_entries_from_todo("") is None


# ── TurnEmitter: AIAgent callbacks → Centaur NDJSON ─────────────────────────────


def test_stream_delta_accumulates_and_emits():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    events = _capture(lambda: (em.on_stream_delta("Hel"), em.on_stream_delta("lo")))
    assert events == [
        {"type": "agent_message_chunk", "text": "Hel"},
        {"type": "agent_message_chunk", "text": "lo"},
    ]
    assert em.final_text == "Hello"


def test_stream_delta_empty_dropped():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    assert _capture(lambda: em.on_stream_delta("")) == []


def test_reasoning_emits_thought_chunk():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    events = _capture(lambda: em.on_reasoning("hmm"))
    assert events == [{"type": "agent_thought_chunk", "text": "hmm"}]


def test_tool_progress_only_started_emits():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    events = _capture(lambda: em.on_tool_progress("tool.completed", "read", None, {}))
    assert events == []


def test_tool_progress_parses_string_args():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    events = _capture(
        lambda: em.on_tool_progress("tool.started", "read", None, '{"path":"x"}')
    )
    assert events[0]["type"] == "tool_call"
    assert events[0]["name"] == "read"
    assert events[0]["input"] == {"path": "x"}


def test_tool_start_complete_correlation_parallel_same_name():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()

    def run():
        em.on_tool_progress("tool.started", "read_file", None, {"path": "x"})
        em.on_tool_progress("tool.started", "read_file", None, {"path": "y"})
        em.on_step(
            1,
            [
                {"name": "read_file", "result": "BODY-x"},
                {"name": "read_file", "result": "BODY-y"},
            ],
        )

    events = _capture(run)
    starts = [e for e in events if e["type"] == "tool_call"]
    updates = [e for e in events if e["type"] == "tool_call_update"]
    assert len(starts) == 2 and len(updates) == 2
    # FIFO correlation: first start pairs with first completion.
    assert starts[0]["tool_call_id"] == updates[0]["tool_call_id"]
    assert starts[1]["tool_call_id"] == updates[1]["tool_call_id"]
    assert updates[0]["output"] == "BODY-x" and updates[1]["output"] == "BODY-y"
    assert all(u["status"] == "completed" and u["is_error"] is False for u in updates)


def test_step_error_marks_failed():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    events = _capture(lambda: em.on_step(1, [{"name": "web", "error": "timeout"}]))
    assert events[0]["type"] == "tool_call_update"
    assert events[0]["status"] == "failed"
    assert events[0]["is_error"] is True


def test_step_todo_emits_plan():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    result = '{"todos":[{"content":"step1","status":"completed"}]}'
    events = _capture(lambda: em.on_step(1, [{"name": "todo", "result": result}]))
    assert {
        "type": "plan",
        "entries": [{"content": "step1", "priority": "medium", "status": "completed"}],
    } in events


def test_step_ignores_non_list():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    assert _capture(lambda: em.on_step(1, None)) == []


def test_reset_clears_state():
    wrapper = _load_wrapper()
    em = wrapper.TurnEmitter()
    em.on_stream_delta("x")
    em.reset()
    assert em.final_text == ""


# ── Wrapper turn completion semantics ─────────────────────────────────────────


def test_run_turn_fails_closed_when_agent_returns_no_text():
    wrapper = _load_wrapper()

    class FakeAgent:
        def chat(self, _prompt):
            return None

    subject = wrapper.Wrapper()
    subject.agent = FakeAgent()

    events = _capture(
        lambda: subject._run_turn(
            {"type": "user", "message": {"content": [{"type": "text", "text": "hi"}]}}
        )
    )

    assert events == [
        {"type": "error", "message": "hermes returned no assistant output"},
        {
            "type": "turn.failed",
            "error": {"message": "hermes returned no assistant output"},
        },
    ]


def test_run_turn_suppresses_empty_failure_when_interrupted():
    wrapper = _load_wrapper()
    subject = wrapper.Wrapper()

    class FakeAgent:
        def interrupt(self):
            return None

        def chat(self, _prompt):
            subject.request_interrupt()
            return None

    subject.agent = FakeAgent()

    events = _capture(
        lambda: subject._run_turn(
            {"type": "user", "message": {"content": [{"type": "text", "text": "hi"}]}}
        )
    )

    assert events == [{"type": "system", "subtype": "turn_interrupted"}]


def test_run_turn_preserves_interrupt_received_before_turn_reset():
    wrapper = _load_wrapper()
    subject = wrapper.Wrapper()

    class FakeAgent:
        def interrupt(self):
            return None

        def chat(self, _prompt):
            return None

    subject.agent = FakeAgent()
    subject.request_interrupt()

    events = _capture(
        lambda: subject._run_turn(
            {"type": "user", "message": {"content": [{"type": "text", "text": "hi"}]}}
        )
    )

    assert events == [{"type": "system", "subtype": "turn_interrupted"}]


def test_run_turn_uses_streamed_text_when_agent_returns_none():
    wrapper = _load_wrapper()

    class FakeAgent:
        def __init__(self, emitter):
            self.emitter = emitter

        def chat(self, _prompt):
            self.emitter.on_stream_delta("PONG")
            return None

    subject = wrapper.Wrapper()
    subject.agent = FakeAgent(subject.emitter)

    events = _capture(
        lambda: subject._run_turn(
            {"type": "user", "message": {"content": [{"type": "text", "text": "hi"}]}}
        )
    )

    assert events == [
        {"type": "agent_message_chunk", "text": "PONG"},
        {"type": "turn.completed", "stop_reason": "end_turn", "text": "PONG"},
    ]


def test_run_turn_carries_prior_completed_turns_into_next_prompt():
    wrapper = _load_wrapper()

    class FakeAgent:
        def __init__(self):
            self.prompts: list[str] = []

        def chat(self, prompt):
            self.prompts.append(prompt)
            if len(self.prompts) == 1:
                return "SAVED"
            return "HERMES_NONCE_7429"

    agent = FakeAgent()
    subject = wrapper.Wrapper()
    subject.agent = agent

    first = {
        "type": "user",
        "message": {
            "content": [
                {
                    "type": "text",
                    "text": "Remember nonce HERMES_NONCE_7429. Reply exactly SAVED.",
                }
            ]
        },
    }
    second = {
        "type": "user",
        "message": {
            "content": [
                {
                    "type": "text",
                    "text": "What nonce did I give you? Reply only with the nonce.",
                }
            ]
        },
    }

    events = _capture(lambda: (subject._run_turn(first), subject._run_turn(second)))

    assert agent.prompts[0] == "Remember nonce HERMES_NONCE_7429. Reply exactly SAVED."
    assert "Previous conversation in this Hermes runtime" in agent.prompts[1]
    assert "User:\nRemember nonce HERMES_NONCE_7429. Reply exactly SAVED." in agent.prompts[1]
    assert "Assistant:\nSAVED" in agent.prompts[1]
    assert (
        "Current user message:\nWhat nonce did I give you? Reply only with the nonce."
        in agent.prompts[1]
    )
    assert events == [
        {"type": "turn.completed", "stop_reason": "end_turn", "text": "SAVED"},
        {
            "type": "turn.completed",
            "stop_reason": "end_turn",
            "text": "HERMES_NONCE_7429",
        },
    ]
