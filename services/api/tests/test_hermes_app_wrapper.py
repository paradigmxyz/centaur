"""Tests for hermes-app-wrapper — command selection + ACP event translation.

The wrapper imports ``acp`` lazily, so the module loads (and its pure helpers +
``session_update`` translation are exercised) without the sandbox-only
``agent-client-protocol`` package installed.
"""

from __future__ import annotations

import asyncio
import importlib.util
import io
import json
import sys
from pathlib import Path
from types import ModuleType

import pytest

from api.sandbox.config import build_harness_cmd

WRAPPER_PY = Path(__file__).resolve().parents[2] / "sandbox" / "hermes-app-wrapper.py"


def _load_wrapper() -> ModuleType:
    spec = importlib.util.spec_from_file_location("hermes_app_wrapper", WRAPPER_PY)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class _FakeUpdate:
    """Stands in for an ACP session-update pydantic model."""

    def __init__(self, data: dict) -> None:
        self._data = data

    def model_dump(self, **_kwargs: object) -> dict:
        return self._data


class _Opt:
    def __init__(self, kind: str, option_id: str) -> None:
        self.kind = kind
        self.option_id = option_id


def _capture(coro_factory) -> list[dict]:
    """Run an async client driver, capturing emitted NDJSON lines as dicts."""
    wrapper = _load_wrapper()
    buf = io.StringIO()
    real_stdout = sys.stdout

    async def driver() -> None:
        client = wrapper.CentaurHermesClient()
        sys.stdout = buf
        try:
            await coro_factory(client)
        finally:
            sys.stdout = real_stdout

    asyncio.run(driver())
    return [json.loads(line) for line in buf.getvalue().splitlines() if line]


# ── command selection ─────────────────────────────────────────────────────────


def test_build_harness_cmd_selects_hermes_wrapper():
    assert build_harness_cmd("hermes") == ["hermes-app-wrapper"]


def test_hermes_acp_cmd_prefers_console_script(monkeypatch):
    wrapper = _load_wrapper()
    monkeypatch.delenv("HERMES_ACP_COMMAND", raising=False)
    monkeypatch.setattr(
        wrapper, "_hermes_acp_cmd", wrapper._hermes_acp_cmd
    )  # no-op, keep ref
    import shutil

    monkeypatch.setattr(shutil, "which", lambda name: "/usr/local/bin/" + name)
    assert wrapper._hermes_acp_cmd() == ["hermes-acp"]


def test_hermes_acp_cmd_env_override(monkeypatch):
    wrapper = _load_wrapper()
    monkeypatch.setenv("HERMES_ACP_COMMAND", "hermes acp --foo")
    assert wrapper._hermes_acp_cmd() == ["hermes", "acp", "--foo"]


def test_hermes_acp_cmd_module_fallback(monkeypatch):
    wrapper = _load_wrapper()
    monkeypatch.delenv("HERMES_ACP_COMMAND", raising=False)
    import shutil

    monkeypatch.setattr(shutil, "which", lambda name: None)
    cmd = wrapper._hermes_acp_cmd()
    assert cmd[1:] == ["-m", "acp_adapter.entry"]


# ── pure helpers ───────────────────────────────────────────────────────────────


def test_prompt_text_joins_blocks():
    wrapper = _load_wrapper()
    turn = {"message": {"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}}
    assert wrapper._prompt_text(turn) == "a\nb"


def test_prompt_text_empty_defaults_to_continue():
    wrapper = _load_wrapper()
    assert wrapper._prompt_text({}) == "continue"


def test_prompt_text_image_placeholder():
    wrapper = _load_wrapper()
    turn = {"message": {"content": [{"type": "image"}]}}
    assert "image attachment" in wrapper._prompt_text(turn)


def test_tool_output_text_from_content_blocks():
    wrapper = _load_wrapper()
    update = {"content": [{"type": "content", "content": {"type": "text", "text": "done"}}]}
    assert wrapper._tool_output_text(update) == "done"


def test_tool_output_text_raw_output_fallback():
    wrapper = _load_wrapper()
    assert wrapper._tool_output_text({"rawOutput": {"x": 1}}) == json.dumps({"x": 1})


def test_pick_allow_option_prefers_allow_always():
    wrapper = _load_wrapper()
    options = [_Opt("reject_once", "r"), _Opt("allow_once", "a1"), _Opt("allow_always", "a2")]
    assert wrapper._pick_allow_option(options) == "a2"


def test_pick_allow_option_falls_back_to_allow_once():
    wrapper = _load_wrapper()
    options = [_Opt("allow_once", "a1"), _Opt("reject_always", "r")]
    assert wrapper._pick_allow_option(options) == "a1"


def test_pick_allow_option_none_when_only_rejects():
    wrapper = _load_wrapper()
    options = [_Opt("reject_once", "r1"), _Opt("reject_always", "r2")]
    assert wrapper._pick_allow_option(options) is None


# ── session_update translation ─────────────────────────────────────────────────


def test_session_update_message_chunk_accumulates_and_emits():
    wrapper = _load_wrapper()
    client = wrapper.CentaurHermesClient()
    buf = io.StringIO()
    real = sys.stdout

    async def drive():
        sys.stdout = buf
        try:
            await client.session_update(
                "s", _FakeUpdate({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hel"}})
            )
            await client.session_update(
                "s", _FakeUpdate({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "lo"}})
            )
        finally:
            sys.stdout = real

    asyncio.run(drive())
    events = [json.loads(line) for line in buf.getvalue().splitlines() if line]
    assert events == [
        {"type": "agent_message_chunk", "text": "Hel"},
        {"type": "agent_message_chunk", "text": "lo"},
    ]
    assert client.final_text == "Hello"


def test_session_update_thought_chunk():
    events = _capture(
        lambda c: c.session_update(
            "s", _FakeUpdate({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}})
        )
    )
    assert events == [{"type": "agent_thought_chunk", "text": "hmm"}]


def test_session_update_tool_call():
    events = _capture(
        lambda c: c.session_update(
            "s",
            _FakeUpdate(
                {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "t1",
                    "title": "Read file",
                    "kind": "read",
                    "rawInput": {"path": "x"},
                }
            ),
        )
    )
    assert events == [
        {
            "type": "tool_call",
            "tool_call_id": "t1",
            "name": "Read file",
            "kind": "read",
            "input": {"path": "x"},
        }
    ]


def test_session_update_tool_call_update_completed():
    events = _capture(
        lambda c: c.session_update(
            "s",
            _FakeUpdate(
                {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "t1",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "BODY"}}],
                }
            ),
        )
    )
    assert events == [
        {
            "type": "tool_call_update",
            "tool_call_id": "t1",
            "status": "completed",
            "output": "BODY",
            "is_error": False,
        }
    ]


def test_session_update_tool_call_update_in_progress_suppressed():
    events = _capture(
        lambda c: c.session_update(
            "s", _FakeUpdate({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress"})
        )
    )
    assert events == []


def test_session_update_plan():
    entries = [{"content": "step", "priority": "medium", "status": "pending"}]
    events = _capture(
        lambda c: c.session_update("s", _FakeUpdate({"sessionUpdate": "plan", "entries": entries}))
    )
    assert events == [{"type": "plan", "entries": entries}]


def test_session_update_dict_fallback():
    # A raw dict (no model_dump) is tolerated.
    events = _capture(
        lambda c: c.session_update("s", {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "x"}})
    )
    assert events == [{"type": "agent_message_chunk", "text": "x"}]
