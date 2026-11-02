#!/usr/bin/env python3
"""Fetch repo-cache checkouts into a sandbox over HTTP.

Lists the repositories this sandbox's principal may read, clones new ones,
updates existing ones in place, and removes ones it may no longer read.
iron-proxy injects the principal token on requests to repo-cache, so the
sandbox never holds it. Updates happen in place because `git clone --shared`
copies borrow objects from these checkouts.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import urllib.request

REPOSITORY_NAME = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
MANIFEST_NAME = ".repo-cache-fetch.json"


def _env_float(name: str, default: float) -> float:
    try:
        value = float(os.environ.get(name, "").strip() or default)
    except ValueError:
        return default
    return value if value > 0 else default


def _valid_repository(repo: object) -> bool:
    return (
        isinstance(repo, str)
        and REPOSITORY_NAME.match(repo) is not None
        and not any(part in {".", ".."} for part in repo.split("/"))
    )


def list_repositories(base_url: str) -> list[str]:
    with urllib.request.urlopen(f"{base_url}/repos", timeout=30) as response:
        payload = json.load(response)
    repositories = payload.get("repositories") if isinstance(payload, dict) else None
    if not isinstance(repositories, list):
        raise RuntimeError("repo-cache returned an invalid repository list")
    return sorted({repo for repo in repositories if _valid_repository(repo)})


def _git(*args: str) -> str:
    env = {**os.environ, "GIT_TERMINAL_PROMPT": "0"}
    try:
        result = subprocess.run(
            ["git", *args], check=True, text=True, capture_output=True, env=env
        )
    except subprocess.CalledProcessError as exc:
        detail = exc.stderr.strip()
        raise RuntimeError(f"git {args[0] if args else ''} failed: {detail}") from exc
    return result.stdout.strip()


def sync_repository(base_url: str, dest: Path, repo: str) -> None:
    url = f"{base_url}/{repo}.git"
    path = dest / repo
    if (path / ".git").is_dir():
        repo_git = ("-C", str(path))
        _git(*repo_git, "remote", "set-url", "origin", url)
        _git(
            *repo_git,
            "-c",
            "gc.auto=0",
            "fetch",
            "--quiet",
            "--force",
            "--tags",
            "origin",
            "HEAD",
        )
        if _git(*repo_git, "rev-parse", "HEAD") != _git(
            *repo_git, "rev-parse", "FETCH_HEAD"
        ):
            _git(*repo_git, "checkout", "--quiet", "--force", "--detach", "FETCH_HEAD")
            _git(*repo_git, "clean", "-fdq")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.fetch-tmp")
    shutil.rmtree(tmp, ignore_errors=True)
    _git("clone", "--quiet", url, str(tmp))
    _git("-C", str(tmp), "config", "gc.auto", "0")
    shutil.rmtree(path, ignore_errors=True)
    tmp.replace(path)


def _managed_repositories(dest: Path) -> list[str]:
    try:
        managed = json.loads((dest / MANIFEST_NAME).read_text())
    except (OSError, ValueError):
        return []
    if not isinstance(managed, list):
        return []
    return [repo for repo in managed if _valid_repository(repo)]


def _remove_repository(dest: Path, repo: str) -> None:
    path = dest / repo
    shutil.rmtree(path, ignore_errors=True)
    try:
        path.parent.rmdir()
    except OSError:
        pass


def sync_once(base_url: str, dest: Path) -> None:
    repositories = list_repositories(base_url)
    dest.mkdir(parents=True, exist_ok=True)
    for repo in set(_managed_repositories(dest)) - set(repositories):
        print(f"repo-cache: removing {repo}", file=sys.stderr)
        _remove_repository(dest, repo)
    failures = []
    for repo in repositories:
        try:
            sync_repository(base_url, dest, repo)
        except RuntimeError as exc:
            failures.append(f"{repo}: {exc}")
    manifest = dest / MANIFEST_NAME
    tmp = manifest.with_name(f"{MANIFEST_NAME}.tmp")
    tmp.write_text(json.dumps(repositories))
    tmp.replace(manifest)
    if failures:
        raise RuntimeError("; ".join(failures))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dest", type=Path, default=Path.home() / "github")
    parser.add_argument("--watch", action="store_true", help="keep checkouts fresh")
    args = parser.parse_args(argv)
    base_url = os.environ.get("CENTAUR_REPO_CACHE_URL", "").strip().rstrip("/")
    if not base_url:
        print("CENTAUR_REPO_CACHE_URL is not set", file=sys.stderr)
        return 1

    if args.watch:
        interval = _env_float("CENTAUR_REPO_CACHE_FETCH_INTERVAL_SECONDS", 30.0)
        while True:
            time.sleep(interval)
            try:
                sync_once(base_url, args.dest)
            except Exception as exc:  # keep watching through transient failures
                print(f"warning: repo-cache fetch failed: {exc}", file=sys.stderr)

    # The per-sandbox proxy may still be starting or syncing its principal
    # config, so retry before failing the sandbox.
    attempts = int(_env_float("CENTAUR_REPO_CACHE_FETCH_ATTEMPTS", 30))
    for attempt in range(1, attempts + 1):
        try:
            sync_once(base_url, args.dest)
            return 0
        except Exception as exc:
            print(f"repo-cache fetch attempt {attempt} failed: {exc}", file=sys.stderr)
            if attempt < attempts:
                time.sleep(2)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
