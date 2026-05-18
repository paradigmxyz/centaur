from __future__ import annotations

from types import SimpleNamespace

from api.sandbox.prompt_assembly import assemble_prompt


def test_assemble_prompt_without_persona_or_overlay() -> None:
    prompt = assemble_prompt(None, base_prompt="base prompt")

    assert prompt.startswith("[Active deployment]\n|Persona: none - base centaur identity")
    assert "|Overlay loaded: no" in prompt
    assert "base prompt" in prompt
    # No persona => no persona-precedence note injected.
    assert "Persona overlay loaded" not in prompt
    # The agent must always see the env vars and runtime command it is supposed
    # to use to introspect itself; lock that into the assembled prompt so a
    # future edit to prompt_assembly cannot drop them silently.
    assert '$AGENT_PERSONA' in prompt
    assert '$CENTAUR_OVERLAY_DIR' in prompt
    assert "call agent runtime '?key='\"$CENTAUR_THREAD_KEY\"" in prompt


def test_assemble_prompt_with_persona_announces_persona_precedence() -> None:
    """Generic mechanism: any persona override gets a deployment-block note
    declaring that the persona overlay wins on routing/tool/voice conflicts.
    The note is persona-agnostic — no specific persona name or tool name
    appears so this works for every current and future persona."""
    persona = SimpleNamespace(engine="amp", prompt_content="persona body")
    prompt = assemble_prompt("anything", base_prompt="base", persona_info=persona)

    assert "Persona overlay loaded" in prompt
    # Sanity: the note must NOT name any particular persona or tool — it's a
    # generic mechanism that future personas inherit automatically.
    assert "invest" not in prompt.split("---")[0].lower()
    assert "invest_research" not in prompt.split("---")[0]


def test_assemble_prompt_handles_non_utf8_overlay_file(tmp_path) -> None:
    overlay_prompt = tmp_path / "SYSTEM_PROMPT.md"
    overlay_prompt.write_bytes(b"valid header\n\xfe\xfe trailing bytes")

    prompt = assemble_prompt(
        None,
        base_prompt="base prompt",
        overlay_prompt_path=overlay_prompt,
        api_overlay_dir=tmp_path,
    )

    assert "valid header" in prompt
    assert "|Overlay loaded: yes" in prompt


def test_assemble_prompt_includes_overlay_and_persona_prompt(tmp_path) -> None:
    overlay_prompt = tmp_path / "SYSTEM_PROMPT.md"
    overlay_prompt.write_text("overlay prompt")
    persona_dir = tmp_path / "persona"
    persona_dir.mkdir()
    (persona_dir / "CUSTOM.md").write_text("persona prompt")
    persona = SimpleNamespace(
        engine="amp",
        prompt_file="CUSTOM.md",
        tool_dir=persona_dir,
        prompt_content="fallback prompt",
    )

    prompt = assemble_prompt(
        "invest",
        base_prompt="base prompt",
        overlay_prompt_path=overlay_prompt,
        persona_info=persona,
        api_overlay_dir=tmp_path,
        sandbox_overlay_dir="/home/agent/overlay/org",
    )

    assert prompt.startswith("[Active deployment]\n|Persona: invest (engine: amp)")
    assert "|Overlay loaded: yes" in prompt
    assert "|Overlay mount (sandbox): /home/agent/overlay/org" in prompt
    assert "base prompt" in prompt
    assert "overlay prompt" in prompt
    assert "persona prompt" in prompt
    assert "fallback prompt" not in prompt
