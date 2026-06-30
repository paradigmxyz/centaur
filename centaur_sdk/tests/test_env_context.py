"""The sandbox tool runner no longer sets ToolContext; the SDK derives it from env.

When no context is set via ``set_tool_context`` (the in-process server path), the
tool subprocess reads ``CENTAUR_TOOL_NAME`` / ``CENTAUR_THREAD_KEY`` from the
environment so ``secret()`` / ``current_thread_key()`` keep working.
"""

from __future__ import annotations

import pytest

from centaur_sdk.tool_sdk import (
    ToolContext,
    current_thread_key,
    get_tool_context,
    set_tool_context,
)


def test_get_tool_context_falls_back_to_env(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CENTAUR_TOOL_NAME", "cl_dapp_api")
    monkeypatch.setenv("CENTAUR_THREAD_KEY", "thread-123")

    ctx = get_tool_context()
    assert ctx.name == "cl_dapp_api"
    assert ctx.thread_key == "thread-123"
    assert current_thread_key() == "thread-123"


def test_set_context_takes_precedence_over_env(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CENTAUR_TOOL_NAME", "from-env")
    token = set_tool_context(ToolContext(name="from-context"))
    try:
        assert get_tool_context().name == "from-context"
    finally:
        from centaur_sdk.tool_sdk import reset_tool_context

        reset_tool_context(token)


def test_no_context_and_no_env_raises(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("CENTAUR_TOOL_NAME", raising=False)
    monkeypatch.delenv("CENTAUR_THREAD_KEY", raising=False)
    monkeypatch.delenv("CENTAUR_CONTAINER_ID", raising=False)
    with pytest.raises(LookupError):
        get_tool_context()
