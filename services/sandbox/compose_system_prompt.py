#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path


SEPARATOR = "\n\n---\n\n"
OVERLAY_PROMPT = Path("services/sandbox/SYSTEM_PROMPT.md")
PERSONA_PROMPT = Path("AGENTS_PERSONA.md")

# Overlay hooks are named bash scripts under this repo-relative directory. Only
# repos listed in OVERLAY_REPO_DIRS_ENV (configured overlay sources, scoped to
# the session's repo-cache access) may run them.
OVERLAY_HOOKS_DIR = Path("services/sandbox/hooks")
OVERLAY_REPO_DIRS_ENV = "CENTAUR_OVERLAY_REPO_DIRS"
FIRST_TURN_PROMPT_HOOK = "first-turn-prompt.sh"
HOOK_TIMEOUT_ENV = "CENTAUR_FIRST_TURN_PROMPT_HOOK_TIMEOUT_SECONDS"
DEFAULT_HOOK_TIMEOUT_SECONDS = 10.0
HOOK_MAX_BYTES_ENV = "CENTAUR_FIRST_TURN_PROMPT_HOOK_MAX_BYTES"
DEFAULT_HOOK_MAX_BYTES = 64 * 1024


def _warn(message: str) -> None:
    print(f"warning: {message}", file=sys.stderr)


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


def _positive_env_number(name: str, default: float) -> float:
    raw = os.environ.get(name, "").strip()
    if not raw:
        return default
    try:
        value = float(raw)
    except ValueError:
        _warn(f"ignoring invalid {name}={raw!r}")
        return default
    if value <= 0:
        _warn(f"ignoring non-positive {name}={raw!r}")
        return default
    return value


def _overlay_repo_dirs() -> list[Path]:
    raw = os.environ.get(OVERLAY_REPO_DIRS_ENV, "")
    return [Path(entry) for entry in raw.split(":") if entry.strip()]


def _overlay_hooks(name: str) -> list[tuple[Path, Path]]:
    """Return (repo_dir, hook) pairs for a named hook, in overlay order."""
    hooks: list[tuple[Path, Path]] = []
    for repo_dir in _overlay_repo_dirs():
        hook = repo_dir / OVERLAY_HOOKS_DIR / name
        if not hook.is_file():
            continue
        # A hook must live inside its overlay repo, not behind a symlink to an
        # arbitrary (possibly agent-writable) path.
        if not hook.resolve().is_relative_to(repo_dir.resolve()):
            _warn(f"skipping hook {hook}: resolves outside {repo_dir}")
            continue
        hooks.append((repo_dir, hook))
    return hooks


def _run_prompt_hook(
    hook: Path, repo_dir: Path, timeout: float, max_bytes: int
) -> str | None:
    """Run a prompt hook with bash from its repo root; stdout is the fragment.

    A failing, slow, or oversized hook is reported and skipped so it cannot
    block the turn. The hook's stderr passes through to the sandbox log.
    """
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            ["bash", str(hook)],
            cwd=repo_dir,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        _warn(f"hook {hook} failed to start: {error}")
        return None
    try:
        stdout, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        # Kill the hook's whole process group so children cannot outlive it.
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        _warn(f"hook {hook} timed out after {timeout:g}s")
        return None
    elapsed = time.monotonic() - started
    if process.returncode != 0:
        _warn(f"hook {hook} exited with status {process.returncode}")
        return None
    if len(stdout) > max_bytes:
        _warn(f"hook {hook} printed {len(stdout)} bytes, over the {max_bytes} limit")
        return None
    print(
        f"hook {hook}: {len(stdout)} bytes in {elapsed:.2f}s",
        file=sys.stderr,
    )
    output = stdout.decode("utf-8", errors="replace")
    return output if output.strip() else None


def _first_turn_prompt_fragments() -> list[str]:
    timeout = _positive_env_number(HOOK_TIMEOUT_ENV, DEFAULT_HOOK_TIMEOUT_SECONDS)
    max_bytes = int(_positive_env_number(HOOK_MAX_BYTES_ENV, DEFAULT_HOOK_MAX_BYTES))
    fragments: list[str] = []
    for repo_dir, hook in _overlay_hooks(FIRST_TURN_PROMPT_HOOK):
        output = _run_prompt_hook(hook, repo_dir, timeout, max_bytes)
        if output is not None:
            fragments.append(output)
    return fragments


def compose_system_prompt(
    *,
    home_dir: Path,
    target_prompt: Path,
    repo_mount: Path,
    first_turn_prompt_hooks: bool = False,
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

    if first_turn_prompt_hooks:
        fragments.extend(_first_turn_prompt_fragments())

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
        "--first-turn-prompt-hooks",
        action="store_true",
        help="also append the output of overlay first-turn-prompt.sh hooks",
    )
    args = parser.parse_args()

    home_dir = Path(args.home_dir)
    compose_system_prompt(
        home_dir=home_dir,
        target_prompt=Path(args.target_prompt),
        repo_mount=Path(args.repo_mount) if args.repo_mount else home_dir / "github",
        first_turn_prompt_hooks=args.first_turn_prompt_hooks,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
