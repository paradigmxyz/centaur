#!/usr/bin/env python3
"""Keep configured GitHub repositories synced into the Centaur repo cache."""

from __future__ import annotations

from datetime import datetime, timezone
import glob
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time
from urllib.parse import urlsplit

REPOSITORY_VISIBILITIES = {"private", "public"}
PUBLIC_REPOSITORY_VISIBILITY = "public"
PRIVATE_REPOSITORY_VISIBILITY = "private"


def _split_words(value: str) -> list[str]:
    return [part for part in value.split() if part]


def _repository_refs(value: str) -> dict[str, str]:
    refs = {}
    for entry in _split_words(value):
        if "=" not in entry:
            continue
        repo, ref = entry.split("=", 1)
        if repo and ref:
            refs[repo] = ref
    return refs


def _normalize_repository_visibility(value: str | None) -> str:
    visibility = (value or "").strip().lower()
    return visibility if visibility in REPOSITORY_VISIBILITIES else "private"


def _repository_visibilities(value: str, repositories: list[str]) -> dict[str, str]:
    visibilities = {repo: "private" for repo in repositories}
    for entry in _split_words(value):
        if "=" not in entry:
            continue
        repo, visibility = entry.split("=", 1)
        if repo:
            visibilities[repo] = _normalize_repository_visibility(visibility)
    return visibilities


def _repository_clone_urls(value: str) -> dict[str, str]:
    if not value.strip():
        return {}
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError as exc:
        raise ValueError("REPOSITORY_CLONE_URLS must be a JSON object") from exc
    if not isinstance(parsed, dict):
        raise ValueError("REPOSITORY_CLONE_URLS must be a JSON object")
    return {
        str(repo).strip(): str(clone_url).strip()
        for repo, clone_url in parsed.items()
        if str(repo).strip() and str(clone_url).strip()
    }


def _atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.tmp")
    tmp.write_text(content)
    tmp.replace(path)


def _remove_path(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()


def _relative_symlink_target(source: Path, link: Path) -> str:
    return os.path.relpath(source, start=link.parent)


class RepoCacheSync:
    def __init__(
        self,
        *,
        cache_dir: Path,
        repositories: list[str],
        repository_refs: dict[str, str],
        repository_visibilities: dict[str, str],
        repository_clone_urls: dict[str, str],
        sync_interval_seconds: float,
        git_username: str,
        git_token_file: Path,
    ) -> None:
        self.cache_dir = cache_dir
        self.repositories = repositories
        self.repository_refs = repository_refs
        self.repository_visibilities = repository_visibilities
        self.repository_clone_urls = repository_clone_urls
        self.sync_interval_seconds = sync_interval_seconds
        self.git_username = git_username
        self.git_token_file = git_token_file
        self.git_env: dict[str, str] | None = None
        self.ready_file = self.cache_dir / ".repo-cache-ready"

    def repository_visibility(self, repo: str) -> str:
        return _normalize_repository_visibility(self.repository_visibilities.get(repo))

    def repository_target(self, repo: str) -> Path:
        return self.cache_dir / self.repository_visibility(repo) / repo

    def legacy_repository_path(self, repo: str) -> Path:
        return self.cache_dir / repo

    def alternate_repository_target(self, repo: str) -> Path:
        visibility = self.repository_visibility(repo)
        alternate = (
            PRIVATE_REPOSITORY_VISIBILITY
            if visibility == PUBLIC_REPOSITORY_VISIBILITY
            else PUBLIC_REPOSITORY_VISIBILITY
        )
        return self.cache_dir / alternate / repo

    def migrate_existing_checkout(self, repo: str, target: Path) -> None:
        if (target / ".git").is_dir():
            return
        for candidate in [
            self.legacy_repository_path(repo),
            self.alternate_repository_target(repo),
        ]:
            if candidate == target or candidate.is_symlink() or not (candidate / ".git").is_dir():
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            _remove_path(target)
            candidate.replace(target)
            return

    def update_legacy_link(self, repo: str, target: Path) -> None:
        link = self.legacy_repository_path(repo)
        if link.is_symlink():
            current = os.readlink(link)
            expected = _relative_symlink_target(target, link)
            if current == expected:
                return
            link.unlink()
        elif link.exists():
            _remove_path(link)
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to(_relative_symlink_target(target, link))

    def remove_stale_visibility_target(self, repo: str) -> None:
        stale = self.alternate_repository_target(repo)
        if stale.exists() or stale.is_symlink():
            _remove_path(stale)

    def replace_checkout(self, target: Path, replacement: Path) -> None:
        if not target.exists() and not target.is_symlink():
            replacement.replace(target)
            return
        previous = target.with_name(f"{target.name}.previous")
        _remove_path(previous)
        target.replace(previous)
        try:
            replacement.replace(target)
        except Exception:
            previous.replace(target)
            raise
        _remove_path(previous)

    @classmethod
    def from_env(cls) -> RepoCacheSync:
        interval = os.environ.get("SYNC_INTERVAL_SECONDS", "").strip()
        try:
            sync_interval_seconds = float(interval) if interval else 30.0
        except ValueError:
            sync_interval_seconds = 30.0
        if sync_interval_seconds <= 0:
            sync_interval_seconds = 30.0
        repositories = _split_words(os.environ.get("REPOSITORIES", ""))

        return cls(
            cache_dir=Path(os.environ.get("REPO_CACHE_DIR", "/cache")),
            repositories=repositories,
            repository_refs=_repository_refs(os.environ.get("REPOSITORY_REFS", "")),
            repository_visibilities=_repository_visibilities(
                os.environ.get("REPOSITORY_VISIBILITIES", ""), repositories
            ),
            repository_clone_urls=_repository_clone_urls(
                os.environ.get("REPOSITORY_CLONE_URLS", "")
            ),
            sync_interval_seconds=sync_interval_seconds,
            git_username=(
                os.environ.get("GIT_USERNAME", "").strip() or "x-access-token"
            ),
            git_token_file=Path(
                os.environ.get("GIT_TOKEN_FILE", "").strip()
                or os.environ.get("GITHUB_TOKEN_FILE", "/github-token/token")
            ),
        )

    def repository_clone_url(self, repo: str) -> str:
        clone_url = self.repository_clone_urls.get(repo) or f"https://github.com/{repo}.git"
        parsed = urlsplit(clone_url)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname:
            raise ValueError(f"clone URL for {repo} must be an HTTP or HTTPS URL")
        if parsed.username is not None or parsed.password is not None:
            raise ValueError(f"clone URL for {repo} must not contain credentials")
        if parsed.query or parsed.fragment:
            raise ValueError(f"clone URL for {repo} must not contain a query or fragment")
        return clone_url

    def _git_env(self) -> dict[str, str]:
        env = os.environ.copy()
        env["GIT_TERMINAL_PROMPT"] = "0"
        if (
            self.git_token_file.is_file()
            and self.git_token_file.stat().st_size > 0
        ):
            askpass = Path("/tmp/git-askpass")
            askpass.write_text(
                "#!/bin/sh\n"
                'case "$1" in\n'
                f"  *Username*) printf '%s\\n' {shlex.quote(self.git_username)} ;;\n"
                f"  *Password*) cat {shlex.quote(str(self.git_token_file))} ;;\n"
                "  *) printf '\\n' ;;\n"
                "esac\n"
            )
            askpass.chmod(0o700)
            env["GIT_ASKPASS"] = str(askpass)
        return env

    def configure_git(self) -> None:
        self._run_git(
            ["config", "--global", "--add", "safe.directory", "*"], "git safe.directory"
        )
        self._run_git(
            ["config", "--global", "init.defaultBranch", "main"],
            "git init.defaultBranch",
        )

    def _run_git(self, args: list[str], label: str) -> subprocess.CompletedProcess[str]:
        if self.git_env is None:
            self.git_env = self._git_env()
        try:
            return subprocess.run(
                ["git", *args],
                check=True,
                text=True,
                env=self.git_env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except subprocess.CalledProcessError as exc:
            stderr = exc.stderr.strip()
            for repo in self.repositories:
                try:
                    clone_url = self.repository_clone_url(repo)
                except ValueError:
                    continue
                stderr = stderr.replace(clone_url, "<redacted-clone-url>")
            detail = f": {stderr}" if stderr else ""
            raise RuntimeError(f"{label} failed{detail}") from exc

    def _git_output(self, repo_path: Path, *args: str) -> str | None:
        try:
            result = self._run_git(["-C", str(repo_path), *args], "git")
        except RuntimeError:
            return None
        return result.stdout.strip() or None

    def _git_ok(self, repo_path: Path, *args: str) -> bool:
        try:
            self._run_git(["-C", str(repo_path), *args], "git")
        except RuntimeError:
            return False
        return True

    def checkout_repo(self, repo: str, target: Path) -> None:
        requested_ref = self.repository_refs.get(repo)
        if requested_ref:
            if self._git_ok(
                target,
                "rev-parse",
                "--verify",
                "--quiet",
                f"origin/{requested_ref}^{{commit}}",
            ):
                self._run_git(
                    [
                        "-C",
                        str(target),
                        "checkout",
                        "-q",
                        "--detach",
                        f"origin/{requested_ref}",
                    ],
                    f"checkout {repo}@origin/{requested_ref}",
                )
            elif self._git_ok(
                target,
                "rev-parse",
                "--verify",
                "--quiet",
                f"{requested_ref}^{{commit}}",
            ):
                self._run_git(
                    ["-C", str(target), "checkout", "-q", "--detach", requested_ref],
                    f"checkout {repo}@{requested_ref}",
                )
            else:
                self._run_git(
                    [
                        "-C",
                        str(target),
                        "-c",
                        "gc.auto=0",
                        "fetch",
                        "--prune",
                        "--tags",
                        "origin",
                        requested_ref,
                    ],
                    f"fetch {repo}@{requested_ref}",
                )
                self._run_git(
                    ["-C", str(target), "checkout", "-q", "--detach", "FETCH_HEAD"],
                    f"checkout {repo}@FETCH_HEAD",
                )
            return

        default_branch = self._git_output(
            target,
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        )
        if default_branch and default_branch.startswith("origin/"):
            default_branch = default_branch.removeprefix("origin/")
        if not default_branch or default_branch == "(unknown)":
            default_branch = "main"
        self._run_git(
            [
                "-C",
                str(target),
                "checkout",
                "-q",
                "-B",
                default_branch,
                f"origin/{default_branch}",
            ],
            f"checkout {repo}@{default_branch}",
        )

    def sync_repo(self, repo: str) -> None:
        repo_url = self.repository_clone_url(repo)
        target = self.repository_target(repo)
        tmp = target.with_name(f"{target.name}.tmp")
        target.parent.mkdir(parents=True, exist_ok=True)
        self.migrate_existing_checkout(repo, target)

        has_checkout = self._git_ok(target, "rev-parse", "--git-dir")
        replace_existing = False
        if has_checkout and self._git_output(target, "remote", "get-url", "origin") != repo_url:
            print(f"Re-cloning {repo} after clone URL changed", flush=True)
            has_checkout = False
            replace_existing = True

        if has_checkout:
            print(f"Updating {repo}", flush=True)
            self._git_ok(target, "config", "gc.auto", "0")
            self._run_git(
                [
                    "-C",
                    str(target),
                    "-c",
                    "gc.auto=0",
                    "fetch",
                    "--prune",
                    "--tags",
                    "origin",
                ],
                f"fetch {repo}",
            )
            self._git_ok(target, "remote", "set-head", "origin", "-a")
            self.checkout_repo(repo, target)
            self._run_git(["-C", str(target), "clean", "-fd"], f"clean {repo}")
            self.remove_stale_visibility_target(repo)
            self.update_legacy_link(repo, target)
            return

        print(f"Cloning {repo}", flush=True)
        for stale_tmp in glob.glob(f"{target}.tmp*"):
            _remove_path(Path(stale_tmp))
        if not replace_existing:
            _remove_path(target)
        self._run_git(["clone", "--quiet", repo_url, str(tmp)], f"clone {repo}")
        self._git_ok(tmp, "config", "gc.auto", "0")
        self._run_git(
            ["-C", str(tmp), "-c", "gc.auto=0", "fetch", "--prune", "--tags", "origin"],
            f"fetch {repo}",
        )
        self._git_ok(tmp, "remote", "set-head", "origin", "-a")
        self.checkout_repo(repo, tmp)
        self._run_git(["-C", str(tmp), "clean", "-fd"], f"clean {repo}")
        self.replace_checkout(target, tmp)
        self.remove_stale_visibility_target(repo)
        self.update_legacy_link(repo, target)

    def repository_fingerprint(self) -> str:
        refs = " ".join(f"{repo}={ref}" for repo, ref in self.repository_refs.items())
        visibilities = " ".join(
            f"{repo}={self.repository_visibilities.get(repo, 'private')}"
            for repo in self.repositories
        )
        clone_url_hashes = " ".join(
            f"{repo}={hashlib.sha256(self.repository_clone_url(repo).encode()).hexdigest()}"
            for repo in self.repositories
        )
        return (
            f"repositories={' '.join(self.repositories)}\n"
            f"repository_refs={refs}\n"
            f"repository_visibilities={visibilities}\n"
            f"repository_clone_url_hashes={clone_url_hashes}\n"
        )

    def write_ready(self) -> None:
        synced_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        _atomic_write(
            self.ready_file, f"{self.repository_fingerprint()}synced_at={synced_at}\n"
        )

    def check_ready(self) -> int:
        try:
            ready_lines = self.ready_file.read_text().splitlines()
        except OSError:
            return 1

        expected_lines = self.repository_fingerprint().splitlines()
        if ready_lines[: len(expected_lines)] != expected_lines:
            return 1
        for repo in self.repositories:
            if not (self.repository_target(repo) / ".git").is_dir():
                return 1
            if not self.legacy_repository_path(repo).is_symlink():
                return 1
        return 0

    def sync_once(self) -> bool:
        sync_ok = True
        for repo in self.repositories:
            try:
                self.sync_repo(repo)
            except Exception as exc:
                print(f"Failed to sync {repo}: {exc}", file=sys.stderr, flush=True)
                sync_ok = False
        if sync_ok:
            self.write_ready()
        else:
            _remove_path(self.ready_file)
        return sync_ok

    def run_forever(self) -> int:
        os.umask(0o022)
        self.configure_git()
        if not self.repositories:
            print(
                "No repositories configured for repo-cache", file=sys.stderr, flush=True
            )
            return 0
        while True:
            self.sync_once()
            time.sleep(self.sync_interval_seconds)


def main() -> int:
    sync = RepoCacheSync.from_env()
    if "--check-ready" in sys.argv[1:]:
        return sync.check_ready()
    return sync.run_forever()


if __name__ == "__main__":
    raise SystemExit(main())
