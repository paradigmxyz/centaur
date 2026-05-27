#!/usr/bin/env python3
from __future__ import annotations

import sys
from pathlib import Path


class HarnessAdapter:
    def prepare_prompt(self, prompt: Path) -> None:
        pass


class AmpAdapter(HarnessAdapter):
    def prepare_prompt(self, prompt: Path) -> None:
        if not prompt.is_file():
            return
        target = prompt.with_name("AGENT.md")
        if not target.resolve().is_relative_to(prompt.parent.resolve()):
            return
        if target.exists() or target.is_symlink():
            target.unlink()
        target.symlink_to(prompt.name)


class CodexAdapter(HarnessAdapter):
    pass


class ClaudeCodeAdapter(HarnessAdapter):
    pass


ADAPTERS = {
    "amp-wrapper": AmpAdapter,
    "codex-app-wrapper": CodexAdapter,
    "claude-app-wrapper": ClaudeCodeAdapter,
}


def main(argv: list[str]) -> int:
    command = Path(argv[1]).name if len(argv) > 1 else ""
    prompt = Path(argv[2]) if len(argv) > 2 else Path.cwd() / "AGENTS.md"
    adapter = ADAPTERS.get(command, HarnessAdapter)()
    adapter.prepare_prompt(prompt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
