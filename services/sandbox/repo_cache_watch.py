#!/usr/bin/env python3
"""Refresh sandbox tools and recompose the system prompt on repo-cache changes."""

from __future__ import annotations

from collections.abc import Callable
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

TOOLS_METADATA_NAME = ".centaur-tools-source.json"
PROMPT_OVERLAY_RELATIVE_PATH = "services/sandbox/SYSTEM_PROMPT.md"
PROMPT_HOME_FILES = (
    "AGENTS_BASE.md",
    "AGENTS.md",
    "AGENTS_OVERLAY.md",
    "AGENTS_PERSONA.md",
)


def _split_paths(value: str) -> list[Path]:
    return [Path(part) for part in value.split(":") if part]


def _env_float(name: str, default: float) -> float:
    value = os.environ.get(name, "").strip()
    if not value:
        return default
    try:
        parsed = float(value)
    except ValueError:
        return default
    return parsed if parsed > 0 else default


def _metadata_sources(metadata: dict[str, object]) -> list[dict[str, object]]:
    sources = metadata.get("sources")
    if isinstance(sources, list) and sources:
        return [source for source in sources if isinstance(source, dict)]
    return [metadata]


def _repo_cache_watches(tool_dirs: list[Path]) -> list[dict[str, str]]:
    watches: list[dict[str, str]] = []
    seen = set()
    for tool_dir in tool_dirs:
        metadata_path = tool_dir / TOOLS_METADATA_NAME
        if not metadata_path.is_file():
            continue
        try:
            metadata = json.loads(metadata_path.read_text())
        except (OSError, json.JSONDecodeError) as exc:
            print(f"warning: failed to read {metadata_path}: {exc}", file=sys.stderr)
            continue
        if not isinstance(metadata, dict):
            continue
        for source in _metadata_sources(metadata):
            if source.get("source") != "repo_cache":
                continue
            repo_cache_repo_path = source.get("repo_cache_repo_path")
            if not repo_cache_repo_path:
                continue
            repo = str(source.get("repo") or repo_cache_repo_path)
            repo_path = str(repo_cache_repo_path)
            key = (repo, repo_path)
            if key in seen:
                continue
            seen.add(key)
            watches.append({"repo": repo, "repo_cache_repo_path": repo_path})
    return sorted(watches, key=lambda watch: (watch["repo"], watch["repo_cache_repo_path"]))


def _git_output(repo_path: str, *args: str) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", repo_path, *args],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return result.stdout.strip() or None


def _repo_cache_fingerprint(tool_dirs: list[Path]) -> str | None:
    watches = _repo_cache_watches(tool_dirs)
    if not watches:
        return None

    entries = []
    for watch in watches:
        repo_path = watch["repo_cache_repo_path"]
        commit = _git_output(repo_path, "rev-parse", "HEAD")
        if commit is None:
            return None
        entries.append(
            {
                "repo": watch["repo"],
                "repo_cache_repo_path": repo_path,
                "commit": commit,
            }
        )
    return json.dumps(entries, sort_keys=True, separators=(",", ":"))


def _refresh_tools() -> int:
    try:
        return subprocess.call(["centaur-tools", "refresh"])
    except OSError as exc:
        print(f"warning: failed to run centaur-tools refresh: {exc}", file=sys.stderr)
        return 1


def _hash_file(path: Path) -> str | None:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def _observability_enabled() -> bool:
    return os.environ.get("CENTAUR_SANDBOX_OBSERVABILITY_ENABLED", "true").lower() != "false"


def _prompt_overlay_paths(repo_mount: Path) -> list[Path]:
    if not repo_mount.is_dir():
        return []
    return sorted(repo_mount.glob(f"*/*/{PROMPT_OVERLAY_RELATIVE_PATH}"))


def _prompt_fingerprint(
    home_dir: Path,
    repo_mount: Path,
    *,
    observability_enabled: bool,
) -> str | None:
    """Fingerprint the composed system prompt's input files.

    Returns ``None`` when none of the inputs exist, so deployments without any
    prompt sources do not keep the recomposition check active.
    """
    entries: dict[str, str | None] = {}
    for name in PROMPT_HOME_FILES:
        entries[name] = _hash_file(home_dir / name)
    for overlay in _prompt_overlay_paths(repo_mount):
        key = f"repo:{overlay.relative_to(repo_mount).as_posix()}"
        entries[key] = _hash_file(overlay)
    entries["observability_enabled"] = str(observability_enabled).lower()

    if not any(
        value is not None
        for key, value in entries.items()
        if key != "observability_enabled"
    ):
        return None
    return json.dumps(entries, sort_keys=True, separators=(",", ":"))


def _compose_system_prompt(home_dir: Path, repo_mount: Path, target: Path) -> int:
    try:
        return subprocess.call(
            [
                "compose-system-prompt",
                "--home-dir",
                str(home_dir),
                "--repo-mount",
                str(repo_mount),
                "--target-prompt",
                str(target),
            ]
        )
    except OSError as exc:
        print(f"warning: failed to run compose-system-prompt: {exc}", file=sys.stderr)
        return 1


def _recompose_prompt(
    home_dir: Path,
    repo_mount: Path,
    target_prompt: Path,
    compose: Callable[[Path, Path, Path], int] = _compose_system_prompt,
) -> bool:
    """Recompose the workspace prompt, replacing it only when content changed."""
    try:
        target_prompt.parent.mkdir(parents=True, exist_ok=True)
        fd, temp_name = tempfile.mkstemp(
            dir=str(target_prompt.parent), prefix=".compose-agents-"
        )
        os.close(fd)
    except OSError as exc:
        print(f"warning: failed to stage prompt recomposition: {exc}", file=sys.stderr)
        return False
    temp_path = Path(temp_name)
    try:
        if compose(home_dir, repo_mount, temp_path) != 0:
            print("warning: compose-system-prompt failed", file=sys.stderr)
            return False
        if temp_path.stat().st_size == 0:
            # compose wrote nothing (no base prompt inputs); leave any existing
            # target untouched rather than clobbering it with an empty file.
            return True
        if (
            target_prompt.is_file()
            and target_prompt.read_bytes() == temp_path.read_bytes()
        ):
            return True
        os.chmod(temp_path, 0o644)
        os.replace(temp_path, target_prompt)
        return True
    except OSError as exc:
        print(
            f"warning: failed to replace prompt at {target_prompt}: {exc}",
            file=sys.stderr,
        )
        return False
    finally:
        if temp_path.exists():
            temp_path.unlink()


def _recompose_if_changed(
    home_dir: Path,
    repo_mount: Path,
    target_prompt: Path,
    applied_fingerprint: str | None,
    *,
    observability_enabled: bool,
    compose: Callable[[Path, Path, Path], int] = _compose_system_prompt,
) -> tuple[str | None, bool]:
    fingerprint = _prompt_fingerprint(
        home_dir, repo_mount, observability_enabled=observability_enabled
    )
    if fingerprint is None or fingerprint == applied_fingerprint:
        return applied_fingerprint, False

    print(
        "repo-cache prompt inputs changed; recomposing the system prompt",
        file=sys.stderr,
    )
    if not _recompose_prompt(home_dir, repo_mount, target_prompt, compose):
        return applied_fingerprint, False
    return fingerprint, True


def _refresh_if_changed(
    tool_dirs: list[Path],
    applied_fingerprint: str | None,
    refresh: Callable[[], int] = _refresh_tools,
) -> tuple[str | None, bool]:
    fingerprint = _repo_cache_fingerprint(tool_dirs)
    if fingerprint is None or fingerprint == applied_fingerprint:
        return applied_fingerprint, False

    print("repo-cache changed; running centaur-tools refresh", file=sys.stderr)
    if refresh() != 0:
        print("warning: centaur-tools refresh failed", file=sys.stderr)
        return applied_fingerprint, False
    return fingerprint, True


def _workspace_dir() -> Path:
    env_workspace = os.environ.get("WORKSPACE_DIR", "").strip()
    if env_workspace:
        return Path(env_workspace)
    return Path.home() / "workspace"


def watch_repo_cache(
    tool_dirs: list[Path],
    *,
    home_dir: Path | None = None,
    repo_mount: Path | None = None,
    workspace_dir: Path | None = None,
    observability_enabled: bool | None = None,
) -> int:
    home_dir = home_dir if home_dir is not None else Path.home()
    repo_mount = repo_mount if repo_mount is not None else home_dir / "github"
    if observability_enabled is None:
        observability_enabled = _observability_enabled()
    workspace = workspace_dir if workspace_dir is not None else _workspace_dir()
    target_prompt = workspace / "AGENTS.md"

    watches = _repo_cache_watches(tool_dirs)
    applied_prompt = _prompt_fingerprint(
        home_dir, repo_mount, observability_enabled=observability_enabled
    )
    if not watches and applied_prompt is None:
        print(
            "repo-cache auto-reload disabled: no repo-cache tool sources or prompt inputs",
            file=sys.stderr,
        )
        return 0

    interval = _env_float("CENTAUR_TOOLS_RELOAD_INTERVAL_SECONDS", 10.0)
    applied_tools = _repo_cache_fingerprint(tool_dirs) if watches else None
    print("repo-cache auto-reload watcher started", file=sys.stderr)

    while True:
        time.sleep(interval)
        if watches:
            applied_tools, _ = _refresh_if_changed(tool_dirs, applied_tools)
        applied_prompt, _ = _recompose_if_changed(
            home_dir,
            repo_mount,
            target_prompt,
            applied_prompt,
            observability_enabled=observability_enabled,
        )


def main() -> int:
    return watch_repo_cache(_split_paths(os.environ.get("TOOL_DIRS", "")))


if __name__ == "__main__":
    raise SystemExit(main())
