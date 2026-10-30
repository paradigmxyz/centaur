#!/usr/bin/env python3
"""Refresh sandbox tools and the system prompt when mounted repo-cache checkouts change."""

from __future__ import annotations

from collections.abc import Callable
import json
import os
from pathlib import Path
import subprocess
import sys
import time

TOOLS_METADATA_NAME = ".centaur-tools-source.json"


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


def _refresh_system_prompt(target_prompt: Path) -> None:
    # Composition only reads the live mounts and rewrites the prompt when it changed.
    try:
        status = subprocess.call(
            ["compose-system-prompt", "--target-prompt", str(target_prompt)]
        )
    except OSError as exc:
        print(f"warning: failed to run compose-system-prompt: {exc}", file=sys.stderr)
        return
    if status != 0:
        print("warning: compose-system-prompt failed", file=sys.stderr)


def watch_repo_cache(tool_dirs: list[Path], target_prompt: Path | None) -> int:
    watch_tools = bool(_repo_cache_watches(tool_dirs))
    if not watch_tools and target_prompt is None:
        print(
            "repo-cache auto-reload disabled: no repo-cache tool sources or prompt",
            file=sys.stderr,
        )
        return 0

    interval = _env_float("CENTAUR_TOOLS_RELOAD_INTERVAL_SECONDS", 10.0)
    applied_fingerprint = _repo_cache_fingerprint(tool_dirs) if watch_tools else None
    print("repo-cache auto-reload watcher started", file=sys.stderr)

    while True:
        time.sleep(interval)
        if watch_tools:
            applied_fingerprint, _ = _refresh_if_changed(tool_dirs, applied_fingerprint)
        if target_prompt is not None:
            _refresh_system_prompt(target_prompt)


def main() -> int:
    workspace_dir = os.environ.get("WORKSPACE_DIR", "")
    return watch_repo_cache(
        _split_paths(os.environ.get("TOOL_DIRS", "")),
        Path(workspace_dir) / "AGENTS.md" if workspace_dir else None,
    )


if __name__ == "__main__":
    raise SystemExit(main())
