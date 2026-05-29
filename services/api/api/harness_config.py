from __future__ import annotations

import os


_DEFAULT_HARNESS_ALIASES: dict[str, str] = {
    "amp": "amp",
    "claude": "claude-code",
    "claude-code": "claude-code",
    "codex": "codex",
    "pi": "pi-mono",
    "pi-mono": "pi-mono",
}


def default_harness() -> str:
    raw = (os.getenv("CENTAUR_DEFAULT_HARNESS") or "codex").strip().lower()
    return _DEFAULT_HARNESS_ALIASES.get(raw, "codex")
