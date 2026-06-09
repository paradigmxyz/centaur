"""Tests for the agent-session title builder used to brand the streamed timeline."""

from __future__ import annotations

from typing import Any
from unittest.mock import AsyncMock

import pytest

from api import workflow_engine


@pytest.mark.asyncio
async def test_title_uses_selector_when_both_persona_and_harness_present(monkeypatch):
    """Selector fully specifies identity; no DB lookup needed."""
    get_active = AsyncMock(return_value=None)
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": "eng", "harness": "amp"}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · eng · amp"
    get_active.assert_not_awaited()


@pytest.mark.asyncio
async def test_title_falls_back_to_active_assignment_for_missing_harness(monkeypatch):
    get_active = AsyncMock(return_value={"persona_id": "eng", "harness": "codex"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": "eng", "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · eng · codex"
    get_active.assert_awaited_once()


@pytest.mark.asyncio
async def test_title_prefers_active_assignment_engine_for_persona_runs(monkeypatch):
    """Persona assignments store the requested persona in harness and the
    actual runtime in engine; the title should display the runtime."""
    get_active = AsyncMock(return_value={"persona_id": "legal", "harness": "legal", "engine": "amp"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": None, "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · legal · amp"


@pytest.mark.asyncio
async def test_title_uses_persona_default_engine_for_fresh_persona_selector(monkeypatch):
    """Fresh persona switches release the old assignment before the session
    opens, so use the persona's configured engine instead of rendering only
    the persona segment."""
    get_active = AsyncMock(return_value=None)
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.setattr(workflow_engine, "_persona_default_engine", lambda persona_id: "amp")
    selector = {"persona_id": "invest", "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · invest · amp"


@pytest.mark.asyncio
async def test_title_omits_duplicate_persona_harness_when_engine_unknown(monkeypatch):
    get_active = AsyncMock(return_value={"persona_id": "legal", "harness": "legal", "engine": None})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.setattr(workflow_engine, "_persona_default_engine", lambda persona_id: None)
    selector = {"persona_id": None, "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · legal"


@pytest.mark.asyncio
async def test_title_falls_back_to_active_assignment_for_missing_persona(monkeypatch):
    get_active = AsyncMock(return_value={"persona_id": "invest", "harness": "amp"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": None, "harness": "amp"}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · invest · amp"


@pytest.mark.asyncio
async def test_title_uses_only_active_assignment_when_selector_empty(monkeypatch):
    get_active = AsyncMock(return_value={"persona_id": "eng", "harness": "codex"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": None, "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · eng · codex"


@pytest.mark.asyncio
async def test_title_drops_segments_for_missing_values(monkeypatch):
    """When neither selector nor active assignment provide a persona, the
    title omits it cleanly (no ``Centaur · None · codex`` artifacts)."""
    get_active = AsyncMock(return_value={"persona_id": None, "harness": "amp"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": None, "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · amp"


@pytest.mark.asyncio
async def test_title_uses_default_harness_when_nothing_is_known(monkeypatch):
    get_active = AsyncMock(return_value=None)
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector: dict[str, Any] = {"persona_id": None, "harness": None}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · codex"


@pytest.mark.asyncio
async def test_title_handles_non_dict_active_assignment(monkeypatch):
    """Defensive: if get_active_assignment returns a non-dict (legacy DB row
    shapes), we don't blow up."""
    get_active = AsyncMock(return_value="not-a-dict")
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector = {"persona_id": None, "harness": "amp"}
    title = await workflow_engine._compute_agent_session_title(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert title == "Centaur · amp"


# ── Per-message header (rendered italic above every assistant message) ─────

@pytest.mark.asyncio
async def test_header_uses_base_when_no_persona_paradigm_claude(monkeypatch):
    """Paradigm + no persona + claude-code → ``base · claude-fable-5``."""
    get_active = AsyncMock(return_value={"persona_id": None, "harness": "claude-code", "engine": "claude-code"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.delenv("CLAUDE_MODEL", raising=False)
    monkeypatch.delenv("CODEX_MODEL", raising=False)
    selector: dict[str, Any] = {"persona_id": None, "harness": None}
    header = await workflow_engine._compute_agent_session_header(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert header == "base · claude-fable-5"


@pytest.mark.asyncio
async def test_header_uses_persona_segment_paradigm_legal_codex(monkeypatch):
    """Paradigm + legal persona + codex → ``legal · codex-gpt-5``."""
    get_active = AsyncMock(return_value={"persona_id": "legal", "harness": "legal", "engine": "codex"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.setenv("CODEX_MODEL", "gpt-5")
    selector: dict[str, Any] = {"persona_id": "legal", "harness": "codex"}
    header = await workflow_engine._compute_agent_session_header(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert header == "legal · codex-gpt-5"


@pytest.mark.asyncio
async def test_header_uses_claude_sonnet_alias_for_tempo(monkeypatch):
    """Tempo + no persona + claude-code with sonnet alias → ``base · claude-sonnet-4-6``."""
    get_active = AsyncMock(return_value={"persona_id": None, "harness": "claude-code", "engine": "claude-code"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.setenv("CLAUDE_MODEL", "sonnet")
    selector: dict[str, Any] = {"persona_id": None, "harness": "claude-code"}
    header = await workflow_engine._compute_agent_session_header(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert header == "base · claude-sonnet-4-6"


@pytest.mark.asyncio
async def test_header_uses_persona_default_engine_when_active_assignment_empty(monkeypatch):
    """Fresh persona switch: no active assignment yet. Falls back to the persona's
    declared default engine so the header is never ``base`` for a known persona."""
    get_active = AsyncMock(return_value=None)
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.setattr(workflow_engine, "_persona_default_engine", lambda persona_id: "codex")
    monkeypatch.setenv("CODEX_MODEL", "gpt-5")
    selector: dict[str, Any] = {"persona_id": "invest", "harness": None}
    header = await workflow_engine._compute_agent_session_header(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert header == "invest · codex-gpt-5"


@pytest.mark.asyncio
async def test_header_passes_through_explicit_claude_model_id(monkeypatch):
    """When CLAUDE_MODEL is already a full id like ``claude-haiku-4-5``,
    pass it through verbatim instead of re-prefixing."""
    get_active = AsyncMock(return_value={"persona_id": None, "harness": "claude-code", "engine": "claude-code"})
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    monkeypatch.setenv("CLAUDE_MODEL", "claude-haiku-4-5")
    selector: dict[str, Any] = {"persona_id": None, "harness": "claude-code"}
    header = await workflow_engine._compute_agent_session_header(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert header == "base · claude-haiku-4-5"


@pytest.mark.asyncio
async def test_header_falls_back_to_engine_name_for_unknown_engine(monkeypatch):
    """Unknown engines render with the engine name verbatim — no surprise output."""
    get_active = AsyncMock(return_value=None)
    monkeypatch.setattr(workflow_engine, "get_active_assignment", get_active)
    selector: dict[str, Any] = {"persona_id": "experimental", "harness": "wasm-runner"}
    header = await workflow_engine._compute_agent_session_header(
        pool=object(), thread_key="slack:T:C:1.0", selector=selector,
    )
    assert header == "experimental · wasm-runner"
