#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
from pathlib import Path


SEPARATOR = "\n\n---\n\n"
OVERLAY_PROMPT = Path("services/sandbox/SYSTEM_PROMPT.md")
OVERLAY_PROMPT_HOOK = Path("services/sandbox/SYSTEM_PROMPT.sh")
HOOK_TIMEOUT_ENV = "CENTAUR_SYSTEM_PROMPT_HOOK_TIMEOUT_SECONDS"
DEFAULT_HOOK_TIMEOUT_SECONDS = 30.0
PERSONA_PROMPT = Path("AGENTS_PERSONA.md")


def _append_file_fragment(fragments: list[str], source: Path) -> bool:
    if not source.is_file():
        return False
    fragments.append(source.read_text())
    return True


def _hook_timeout_seconds() -> float:
    raw = os.environ.get(HOOK_TIMEOUT_ENV, "").strip()
    if not raw:
        return DEFAULT_HOOK_TIMEOUT_SECONDS
    try:
        timeout = float(raw)
    except ValueError:
        print(f"warning: ignoring invalid {HOOK_TIMEOUT_ENV}={raw!r}", file=sys.stderr)
        return DEFAULT_HOOK_TIMEOUT_SECONDS
    return timeout if timeout > 0 else DEFAULT_HOOK_TIMEOUT_SECONDS


def _run_prompt_hook(hook: Path, repo_dir: Path, timeout: float) -> str | None:
    """Run an overlay prompt hook; its stdout is a prompt fragment.

    A failing or slow hook is reported and skipped so it cannot block startup.
    The hook's stderr passes through to the sandbox log.
    """
    try:
        process = subprocess.Popen(
            ["bash", str(hook)],
            cwd=repo_dir,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
    except OSError as error:
        print(
            f"warning: system prompt hook {hook} failed to start: {error}",
            file=sys.stderr,
        )
        return None
    try:
        stdout, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        # Kill the hook's whole process group so children cannot outlive it.
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        print(
            f"warning: system prompt hook {hook} timed out after {timeout:g}s",
            file=sys.stderr,
        )
        return None
    if process.returncode != 0:
        status = process.returncode
        print(
            f"warning: system prompt hook {hook} exited with status {status}",
            file=sys.stderr,
        )
        return None
    return stdout if stdout.strip() else None


def _mounted_overlay_fragments(
    repo_mount: Path, baked_prompt: Path
) -> list[tuple[Path, str]]:
    """Return (source, text) fragments from mounted repos in repo order.

    Each repo contributes its static SYSTEM_PROMPT.md, then the stdout of its
    SYSTEM_PROMPT.sh hook.
    """
    if not repo_mount.is_dir():
        return []
    root_text = baked_prompt.read_text() if baked_prompt.is_file() else None
    timeout = _hook_timeout_seconds()
    fragments: list[tuple[Path, str]] = []
    for repo_dir in sorted(path for path in repo_mount.glob("*/*") if path.is_dir()):
        prompt = repo_dir / OVERLAY_PROMPT
        if prompt.is_file():
            text = prompt.read_text()
            if text != root_text:
                fragments.append((prompt, text))
        hook = repo_dir / OVERLAY_PROMPT_HOOK
        if hook.is_file():
            output = _run_prompt_hook(hook, repo_dir, timeout)
            if output is not None:
                fragments.append((hook, output))
    return fragments


def compose_system_prompt(
    *,
    home_dir: Path,
    target_prompt: Path,
    repo_mount: Path,
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

    for source, text in _mounted_overlay_fragments(repo_mount, baked_prompt):
        resolved = source.resolve()
        if resolved in appended:
            continue
        fragments.append(text)
        appended.add(resolved)

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
    args = parser.parse_args()

    home_dir = Path(args.home_dir)
    compose_system_prompt(
        home_dir=home_dir,
        target_prompt=Path(args.target_prompt),
        repo_mount=Path(args.repo_mount) if args.repo_mount else home_dir / "github",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
