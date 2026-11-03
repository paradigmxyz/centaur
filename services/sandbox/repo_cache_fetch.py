#!/usr/bin/env python3
"""Keep a sandbox's copy of the repo cache matched to its principal's access.

repo-cache lists the repositories the sandbox's principal may read: iron-proxy
injects the principal token on requests to repo-cache, so the sandbox never
holds it. New repositories are unpacked from tar archives, which is far cheaper
than cloning, and existing ones are updated in place with git fetches (so
`git clone --shared` copies keep borrowing their objects). Repositories the
principal may no longer read are deleted.

Modes:
  (default)  copy every readable repository, retrying while the proxy starts
  --watch    keep the copy current and follow access changes
  --bind     apply a newly claimed principal's access before its first turn;
             with --expect-principal, fail unless the proxy already presents
             that principal
"""

from __future__ import annotations

import argparse
from collections.abc import Iterator
from contextlib import contextmanager
import fcntl
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
LOCK_NAME = ".repo-cache-fetch.lock"
RUNTIME_ENV_NAME = ".centaur-runtime-env.json"
CACHE_HEAD_REF = "refs/centaur/repo-cache-head"
MIRROR_REFSPECS = (
    f"+HEAD:{CACHE_HEAD_REF}",
    "+refs/heads/*:refs/heads/*",
    "+refs/remotes/origin/*:refs/remotes/origin/*",
    "+refs/tags/*:refs/tags/*",
)


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


def list_access(base_url: str) -> tuple[str, list[str]]:
    """Return the principal repo-cache sees and the repositories it may read."""
    with urllib.request.urlopen(f"{base_url}/repos", timeout=30) as response:
        payload = json.load(response)
    if not isinstance(payload, dict):
        raise RuntimeError("repo-cache returned an invalid repository list")
    principal = payload.get("principal")
    repositories = payload.get("repositories")
    if not isinstance(principal, str) or not isinstance(repositories, list):
        raise RuntimeError("repo-cache returned an invalid repository list")
    return principal, sorted({repo for repo in repositories if _valid_repository(repo)})


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


def download_repository(base_url: str, dest: Path, repo: str) -> None:
    path = dest / repo
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.fetch-tmp")
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir()
    try:
        with urllib.request.urlopen(f"{base_url}/{repo}.tar", timeout=60) as response:
            tar = subprocess.Popen(
                ["tar", "-x", "--no-same-owner", "-C", str(tmp), "-f", "-"],
                stdin=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert tar.stdin is not None and tar.stderr is not None
            try:
                shutil.copyfileobj(response, tar.stdin, 1024 * 1024)
            finally:
                tar.stdin.close()
                stderr = tar.stderr.read()
                tar.wait()
        if tar.returncode != 0:
            raise RuntimeError(f"unpack {repo} failed: {stderr.decode().strip()}")
        _git("-C", str(tmp), "rev-parse", "--verify", "--quiet", "HEAD^{commit}")
    except BaseException:
        shutil.rmtree(tmp, ignore_errors=True)
        raise
    shutil.rmtree(path, ignore_errors=True)
    tmp.replace(path)


def update_repository(base_url: str, dest: Path, repo: str) -> None:
    """Fetch the cache's refs and check out its HEAD.

    origin keeps pointing at GitHub, so pushes and `git-branch` still target
    the real remote.
    """
    repo_git = ("-C", str(dest / repo))
    head = _git(*repo_git, "rev-parse", "HEAD")
    _git(
        *repo_git,
        "-c",
        "gc.auto=0",
        "fetch",
        "--quiet",
        "--force",
        "--update-head-ok",
        f"{base_url}/{repo}.git",
        *MIRROR_REFSPECS,
    )
    cache_head = _git(*repo_git, "rev-parse", CACHE_HEAD_REF)
    moved_head = _git(*repo_git, "rev-parse", "HEAD")
    if moved_head != head:
        # The fetch moved the checked-out branch but not the worktree.
        _git(*repo_git, "reset", "--quiet", "--hard", "HEAD")
    if moved_head != cache_head:
        _git(*repo_git, "checkout", "--quiet", "--force", "--detach", cache_head)


def _managed_repositories(dest: Path) -> list[str]:
    try:
        managed = json.loads((dest / MANIFEST_NAME).read_text())
    except (OSError, ValueError):
        return []
    if not isinstance(managed, list):
        return []
    return [repo for repo in managed if _valid_repository(repo)]


def _present_repositories(dest: Path) -> list[str]:
    return sorted(
        f"{owner.name}/{repo.name}"
        for owner in dest.glob("*")
        if owner.is_dir() and not owner.is_symlink() and not owner.name.startswith(".")
        for repo in owner.glob("*")
        if not repo.name.startswith(".")
    )


def _remove_repository(dest: Path, repo: str) -> None:
    path = dest / repo
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()
    try:
        path.parent.rmdir()
    except OSError:
        pass


@contextmanager
def _locked(dest: Path, deadline: float | None) -> Iterator[None]:
    """Serialize passes so a watcher pass that listed with the previous
    principal's token cannot add a repository after a bind removed it."""
    dest.mkdir(parents=True, exist_ok=True)
    with (dest / LOCK_NAME).open("a") as lock_file:
        while True:
            try:
                fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if deadline is not None and time.monotonic() >= deadline:
                    raise RuntimeError("timed out waiting for another repo-cache pass")
                time.sleep(0.2)
        yield


def sync_once(
    base_url: str,
    dest: Path,
    *,
    expect_principal: str | None = None,
    update: bool = True,
    deadline: float | None = None,
) -> tuple[bool, list[str]]:
    """Match `dest` to the principal's access.

    Returns whether the set of repositories changed, and the repositories that
    failed to copy or update. Removals happen before anything is added, and a
    failed listing changes nothing.
    """
    with _locked(dest, deadline):
        principal, repositories = list_access(base_url)
        if expect_principal is not None and principal != expect_principal:
            raise RuntimeError(
                f"repo-cache sees principal {principal!r}, expected {expect_principal!r}"
            )
        allowed = set(repositories)
        # Anything the manifest or the directory holds that is no longer
        # readable goes, including leftovers from an interrupted pass.
        stale = (set(_managed_repositories(dest)) | set(_present_repositories(dest))) - allowed
        for repo in sorted(stale):
            print(f"repo-cache: removing {repo}", file=sys.stderr)
            _remove_repository(dest, repo)
        changed = bool(stale)
        failures = []
        for repo in repositories:
            try:
                if not (dest / repo / ".git").is_dir():
                    download_repository(base_url, dest, repo)
                    changed = True
                elif update:
                    update_repository(base_url, dest, repo)
            except Exception as exc:
                failures.append(f"{repo}: {exc}")
        manifest = dest / MANIFEST_NAME
        tmp = manifest.with_name(f"{MANIFEST_NAME}.tmp")
        tmp.write_text(json.dumps(repositories))
        tmp.replace(manifest)
    return changed, failures


def reload_overlays() -> None:
    """Rebuild what the entrypoint derived from the copy: published tools and
    shims, workspace skills, and the composed system prompt.

    The entrypoint records the environment it computed for these steps, so an
    exec'd bind derives exactly what a fresh start would.
    """
    env = dict(os.environ)
    runtime_env = Path.home() / RUNTIME_ENV_NAME
    try:
        recorded = json.loads(runtime_env.read_text())
    except (OSError, ValueError) as exc:
        raise RuntimeError(f"cannot read {runtime_env}: {exc}") from exc
    env.update({key: str(value) for key, value in recorded.items()})
    workspace = Path(env["WORKSPACE_DIR"])
    # Skills are copied into the workspace; drop ones from removed repositories.
    shutil.rmtree(workspace / ".agents" / "skills", ignore_errors=True)
    for command in (
        ["install-tool-shims", "--refresh"],
        [
            "compose-system-prompt",
            "--home-dir",
            str(Path.home()),
            "--target-prompt",
            str(workspace / "AGENTS.md"),
        ],
    ):
        subprocess.run(command, check=True, env=env)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--dest", type=Path, default=Path.home() / "github")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--watch", action="store_true")
    mode.add_argument("--bind", action="store_true")
    parser.add_argument("--expect-principal")
    args = parser.parse_args(argv)
    base_url = os.environ.get("CENTAUR_REPO_CACHE_URL", "").strip().rstrip("/")
    if not base_url:
        print("CENTAUR_REPO_CACHE_URL is not set", file=sys.stderr)
        return 1

    # A failed reload is retried on the next pass even if the set is stable.
    needs_reload = False
    if args.watch:
        interval = _env_float("CENTAUR_REPO_CACHE_FETCH_INTERVAL_SECONDS", 30.0)
        while True:
            time.sleep(interval)
            try:
                changed, failures = sync_once(base_url, args.dest)
                needs_reload |= changed
                if failures:
                    print(f"warning: repo-cache fetch failed: {'; '.join(failures)}", file=sys.stderr)
                if needs_reload:
                    reload_overlays()
                    needs_reload = False
            except Exception as exc:  # keep watching through transient failures
                print(f"warning: repo-cache fetch failed: {exc}", file=sys.stderr)

    if args.bind:
        # The proxy may still be applying the claimed principal's config.
        deadline = time.monotonic() + _env_float("CENTAUR_REPO_CACHE_BIND_TIMEOUT_SECONDS", 30)
        while True:
            try:
                # Copies of newly readable repositories that fail here are
                # retried by the watcher; only removals must finish now.
                changed, failures = sync_once(
                    base_url,
                    args.dest,
                    expect_principal=args.expect_principal,
                    update=False,
                    deadline=deadline,
                )
                needs_reload |= changed
                if failures:
                    print(f"warning: repo-cache bind skipped: {'; '.join(failures)}", file=sys.stderr)
                agent_repo = os.environ.get("AGENT_REPO", "").strip()
                if agent_repo and not (args.dest / agent_repo / ".git").is_dir():
                    # The workspace is a clone of it; it cannot be kept.
                    print(f"repo-cache bind failed: AGENT_REPO {agent_repo} is not readable", file=sys.stderr)
                    return 1
                if needs_reload:
                    reload_overlays()
                return 0
            except Exception as exc:
                if time.monotonic() >= deadline:
                    print(f"repo-cache bind failed: {exc}", file=sys.stderr)
                    return 1
                time.sleep(0.5)

    # The per-sandbox proxy may still be starting or syncing its principal
    # config, so retry before failing the sandbox.
    attempts = int(_env_float("CENTAUR_REPO_CACHE_FETCH_ATTEMPTS", 30))
    for attempt in range(1, attempts + 1):
        try:
            _, failures = sync_once(base_url, args.dest, update=False)
            if not failures:
                return 0
            raise RuntimeError("; ".join(failures))
        except Exception as exc:
            print(f"repo-cache fetch attempt {attempt} failed: {exc}", file=sys.stderr)
            if attempt < attempts:
                time.sleep(2)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
