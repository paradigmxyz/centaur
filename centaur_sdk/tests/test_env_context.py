"""The sandbox tool runner no longer binds a ToolContext; current_thread_key()
falls back to the CENTAUR_THREAD_KEY the sandbox sets in the environment.

get_tool_context() must keep raising LookupError when nothing is bound: tools
use "a context is bound" as the signal that ctx.secrets is authoritative, so an
env-derived context with empty secrets would wrongly suppress the secret()
backend fallback (e.g. tools/research/websearch/client.py).
"""

from __future__ import annotations

import pytest

from centaur_sdk.tool_sdk import (
    ToolContext,
    current_thread_key,
    get_tool_context,
    reset_tool_context,
    set_tool_context,
)


def test_get_tool_context_raises_without_binding_even_with_env(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CENTAUR_TOOL_NAME", "cl_dapp_api")
    monkeypatch.setenv("CENTAUR_THREAD_KEY", "thread-123")
    with pytest.raises(LookupError):
        get_tool_context()


def test_current_thread_key_falls_back_to_env(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CENTAUR_THREAD_KEY", "thread-123")
    assert current_thread_key() == "thread-123"


def test_bound_context_thread_key_takes_precedence_over_env(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CENTAUR_THREAD_KEY", "from-env")
    token = set_tool_context(ToolContext(name="t", thread_key="from-context"))
    try:
        assert current_thread_key() == "from-context"
    finally:
        reset_tool_context(token)


def test_current_thread_key_raises_without_context_or_env(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("CENTAUR_THREAD_KEY", raising=False)
    with pytest.raises(RuntimeError):
        current_thread_key()
