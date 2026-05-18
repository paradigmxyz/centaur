"""System prompt assembly shared by cold and warm sandbox paths."""

from __future__ import annotations

from pathlib import Path
from typing import Any


def _read_text(path: Path) -> str:
    # Persona/overlay files are author-controlled markdown but may include
    # smart-quote variants from copy/paste; never let a single bad byte fail
    # the whole sandbox spawn.
    return path.read_text(encoding="utf-8", errors="replace")


def _persona_line(persona: str | None, persona_info: Any | None) -> str:
    if not persona:
        return "Persona: none - base centaur identity"
    engine = getattr(persona_info, "engine", None) or "unknown"
    return f"Persona: {persona} (engine: {engine})"


def _persona_prompt(persona_info: Any | None) -> str:
    if persona_info is None:
        return ""
    prompt_file = getattr(persona_info, "prompt_file", None) or "PROMPT.md"
    tool_dir = getattr(persona_info, "tool_dir", None)
    if tool_dir is None:
        return getattr(persona_info, "prompt_content", "") or ""
    prompt_path = Path(tool_dir) / prompt_file
    if prompt_path.is_file():
        return _read_text(prompt_path)
    return getattr(persona_info, "prompt_content", "") or ""


def _active_deployment_block(
    persona: str | None,
    *,
    persona_info: Any | None,
    overlay_loaded: bool,
    sandbox_overlay_dir: str | None,
) -> str:
    lines = [
        "[Active deployment]",
        f"|{_persona_line(persona, persona_info)}",
        f"|Overlay loaded: {'yes' if overlay_loaded else 'no'}",
    ]
    if sandbox_overlay_dir:
        lines.append(f"|Overlay mount (sandbox): {sandbox_overlay_dir}")
    else:
        lines.append("|Overlay mount (sandbox): none")
    if persona:
        lines.append(
            "|Persona overlay loaded: its instructions override generic "
            "base-prompt guidance on routing, tool choice, voice, and "
            "output shape. Read the overlay before acting on any base "
            "default."
        )
    lines.extend(
        [
            "|To verify at runtime, run any of:",
            '|  echo "$AGENT_PERSONA"',
            '|  echo "$CENTAUR_OVERLAY_DIR"',
            '|  ls "$CENTAUR_OVERLAY_DIR"',
            '|  call agent runtime \'?key=\'"$CENTAUR_THREAD_KEY"',
            '|Never claim "no persona" or "no overlay loaded" without checking these.',
        ]
    )
    return "\n".join(lines)


def assemble_prompt(
    persona: str | None,
    *,
    base_prompt: str,
    overlay_prompt_path: Path | None = None,
    persona_info: Any | None = None,
    api_overlay_dir: Path | None = None,
    sandbox_overlay_dir: str | None = None,
) -> str:
    """Build the effective sandbox system prompt.

    The active deployment block is intentionally the first content the agent sees.
    Base, org overlay, and persona prompts then remain as behavior context instead
    of competing to be the source of truth about the current runtime.
    """
    overlay_prompt = ""
    if overlay_prompt_path is not None and overlay_prompt_path.is_file():
        overlay_prompt = _read_text(overlay_prompt_path)

    overlay_loaded = bool(
        overlay_prompt
        or (api_overlay_dir is not None and api_overlay_dir.exists())
        or sandbox_overlay_dir
    )
    sections = [
        _active_deployment_block(
            persona,
            persona_info=persona_info,
            overlay_loaded=overlay_loaded,
            sandbox_overlay_dir=sandbox_overlay_dir,
        ),
        base_prompt,
    ]
    if overlay_prompt:
        sections.append(overlay_prompt)
    persona_prompt = _persona_prompt(persona_info)
    if persona_prompt:
        sections.append(persona_prompt)
    return "\n\n---\n\n".join(section.rstrip() for section in sections if section.strip()) + "\n"
