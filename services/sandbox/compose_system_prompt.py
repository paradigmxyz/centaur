#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
from pathlib import Path


SEPARATOR = "\n\n---\n\n"
OVERLAY_PROMPT = Path("services/sandbox/SYSTEM_PROMPT.md")
PERSONA_PROMPT = Path("AGENTS_PERSONA.md")


def _append_file_fragment(fragments: list[str], source: Path) -> bool:
    if not source.is_file():
        return False
    fragments.append(source.read_text())
    return True


def _mounted_overlay_prompts(repo_mount: Path, baked_prompt: Path) -> list[Path]:
    if not repo_mount.is_dir():
        return []
    prompts = sorted(repo_mount.glob(f"*/*/{OVERLAY_PROMPT}"))
    if not baked_prompt.is_file():
        return prompts

    root_text = baked_prompt.read_text()
    return [prompt for prompt in prompts if prompt.read_text() != root_text]


def compose_system_prompt(
    *,
    home_dir: Path,
    target_prompt: Path,
    repo_mount: Path,
    session_prompts: list[Path] | None = None,
) -> None:
    base_prompt = home_dir / "AGENTS_BASE.md"
    baked_prompt = home_dir / "AGENTS.md"
    selected_base = base_prompt if base_prompt.is_file() else baked_prompt
    if not selected_base.is_file():
        return

    fragments = [selected_base.read_text()]
    appended: set[Path] = set()

    home_overlay = home_dir / "AGENTS_OVERLAY.md"
    if _append_file_fragment(fragments, home_overlay):
        appended.add(home_overlay.resolve())

    for prompt_path in _mounted_overlay_prompts(repo_mount, baked_prompt):
        if not prompt_path.is_file():
            continue
        resolved = prompt_path.resolve()
        if resolved in appended:
            continue
        if _append_file_fragment(fragments, prompt_path):
            appended.add(resolved)

    # Overlay session-prompt hook output, written by harness-server.
    for prompt_path in session_prompts or []:
        _append_file_fragment(fragments, prompt_path)

    persona_prompt_path = home_dir / PERSONA_PROMPT
    if persona_prompt_path.is_file():
        fragments.append(persona_prompt_path.read_text())

    effective_prompt = SEPARATOR.join(fragments)
    target_prompt.write_text(effective_prompt)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--home-dir", default=os.path.expanduser("~"))
    parser.add_argument("--repo-mount")
    parser.add_argument("--target-prompt", required=True)
    parser.add_argument(
        "--session-prompt",
        action="append",
        default=[],
        help="fragment to append before the persona prompt; repeatable",
    )
    args = parser.parse_args()

    home_dir = Path(args.home_dir)
    compose_system_prompt(
        home_dir=home_dir,
        target_prompt=Path(args.target_prompt),
        repo_mount=Path(args.repo_mount) if args.repo_mount else home_dir / "github",
        session_prompts=[Path(path) for path in args.session_prompt],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
