#!/usr/bin/env python3
"""Prebuild tool virtualenvs into the repo cache for sandboxes to run directly.

Runs as a non-root sidecar next to repo-cache-sync. For every tool project in
the configured tool sources, it builds a relocatable, bytecode-compiled venv
keyed by the tool's content key (see ``install_tool_shims.tool_env_key``) and
publishes it under ``<cache>/<visibility>/.centaur-tool-envs/<key>``.
Sandboxes mount the cache read-only and run a venv whose key matches their own
tool copy; anything else falls back to ``uvx``, so a missing, stale, or failed
build only costs the old first-call latency.

Layout under each visibility root::

    .centaur-tool-envs/
      <key> -> builds/<key>-<ts>      atomically swapped symlink
      builds/<key>-<ts>/venv/         relocatable venv, .pyc precompiled
      builds/<key>-<ts>/.centaur-tool-env.json
      .pyc-store/<sha>                identical .pyc files, hardlinked into envs
      .failures/<key>                 last failure, retried after a backoff

Package files are hardlinked out of the shared uv cache, and identical .pyc
files are hardlinked through ``.pyc-store``, so envs that share dependencies
share their disk.

The builder holds no credentials and never runs tool code; installing a tool
does run its build backend and any dependency build backends, which is why it
runs without the repo-cache GitHub token and as the unprivileged agent user.
"""

from __future__ import annotations

from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from types import ModuleType

VISIBILITIES = ("private", "public")
TOOL_ENVS_DIR_NAME = ".centaur-tool-envs"
UV_CACHE_DIR_NAME = ".centaur-tool-env-cache"
BUILDS_DIR_NAME = "builds"
FAILURES_DIR_NAME = ".failures"
PYC_STORE_DIR_NAME = ".pyc-store"
# compileall's -d prefix. Compiling every env under one fixed path makes .pyc
# bytes for the same package identical across envs so they can share inodes;
# importlib rewrites co_filename to the real source path when loading a .pyc.
PYC_COMPILE_PREFIX = "/centaur-tool-env/site-packages"
TMP_PREFIX = ".tmp-"
ORPHANED_MARKER = ".orphaned"
OUTPUT_TAIL_CHARS = 4000


def _load_shims() -> ModuleType:
    """Share the content key with install-tool-shims, the consumer side."""
    try:
        import install_tool_shims

        return install_tool_shims
    except ImportError:
        pass
    path = shutil.which("install-tool-shims") or "/usr/local/bin/install-tool-shims"
    loader = importlib.machinery.SourceFileLoader("install_tool_shims", path)
    spec = importlib.util.spec_from_loader("install_tool_shims", loader)
    if spec is None:
        raise RuntimeError(f"cannot load install-tool-shims from {path}")
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


shims = _load_shims()


def _env_seconds(name: str, default: float, *, allow_zero: bool = False) -> float:
    raw = os.environ.get(name, "").strip()
    if not raw:
        return default
    try:
        value = float(raw)
    except ValueError:
        return default
    if value < 0 or (value == 0 and not allow_zero):
        return default
    return value


def _tool_sources(value: str) -> list[tuple[str, str]]:
    """Parse ``owner/repo=subdir`` entries; a bare repo means ``tools``."""
    sources = []
    for entry in value.split():
        repo, _, subdir = entry.partition("=")
        if repo:
            sources.append((repo, subdir.strip("/") or "tools"))
    return sources


def _now() -> float:
    return time.time()


def _timestamp(value: float) -> str:
    return datetime.fromtimestamp(value, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _remove(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path, ignore_errors=True)
    else:
        try:
            path.unlink()
        except FileNotFoundError:
            pass


def _stage_ignore(directory: str, names: list[str]) -> set[str]:
    return {
        name
        for name in names
        if shims.tool_env_ignored(name, is_dir=os.path.isdir(os.path.join(directory, name)))
    }


class ToolEnvBuilder:
    def __init__(
        self,
        *,
        cache_dir: Path,
        sources: list[tuple[str, str]],
        interval_seconds: float,
        max_age_seconds: float,
        retention_seconds: float,
        grace_seconds: float,
        failure_backoff_seconds: float,
        build_timeout_seconds: float,
        uv: str = "uv",
        python: str = sys.executable,
    ) -> None:
        self.cache_dir = cache_dir
        self.sources = sources
        self.interval_seconds = interval_seconds
        self.max_age_seconds = max_age_seconds
        self.retention_seconds = retention_seconds
        self.grace_seconds = grace_seconds
        self.failure_backoff_seconds = failure_backoff_seconds
        self.build_timeout_seconds = build_timeout_seconds
        self.uv = uv
        self.python = python
        self.uv_cache_dir = Path(
            os.environ.get("UV_CACHE_DIR") or cache_dir / UV_CACHE_DIR_NAME
        )
        self.allowlist = shims._tool_allowlist()
        self.blocklist = shims._tool_blocklist()
        self.pruned_since_gc = False

    @classmethod
    def from_env(cls) -> ToolEnvBuilder:
        return cls(
            cache_dir=Path(os.environ.get("REPO_CACHE_DIR", "/cache")),
            sources=_tool_sources(os.environ.get("TOOL_ENV_SOURCES", "")),
            interval_seconds=_env_seconds("TOOL_ENV_SYNC_INTERVAL_SECONDS", 60.0),
            max_age_seconds=_env_seconds(
                "TOOL_ENV_MAX_AGE_SECONDS", 86400.0, allow_zero=True
            ),
            retention_seconds=_env_seconds("TOOL_ENV_RETENTION_SECONDS", 7 * 86400.0),
            grace_seconds=_env_seconds("TOOL_ENV_GRACE_SECONDS", 6 * 3600.0),
            failure_backoff_seconds=_env_seconds(
                "TOOL_ENV_FAILURE_BACKOFF_SECONDS", 3600.0
            ),
            build_timeout_seconds=_env_seconds("TOOL_ENV_BUILD_TIMEOUT_SECONDS", 900.0),
            uv=os.environ.get("TOOL_ENV_UV", "uv"),
        )

    # ── discovery ────────────────────────────────────────────────────────────

    def envs_root(self, visibility: str) -> Path:
        return self.cache_dir / visibility / TOOL_ENVS_DIR_NAME

    def tool_projects(self, visibility: str) -> list[Path]:
        projects = []
        for repo, subdir in self.sources:
            checkout = self.cache_dir / visibility / repo
            if not (checkout / ".git").is_dir():
                continue
            for package_dir in shims._tool_package_dirs(checkout / subdir):
                name = package_dir.name
                if self.allowlist is not None and name not in self.allowlist:
                    continue
                if name in self.blocklist:
                    continue
                projects.append(package_dir)
        return projects

    # ── state ────────────────────────────────────────────────────────────────

    def _marker(self, build: Path) -> dict[str, object] | None:
        try:
            marker = json.loads((build / shims.TOOL_ENV_MARKER_NAME).read_text())
        except (OSError, ValueError):
            return None
        return marker if isinstance(marker, dict) else None

    def needs_build(self, root: Path, key: str) -> bool:
        link = root / key
        try:
            build = link.resolve(strict=True)
        except OSError:
            return not self._recently_failed(root, key)
        marker = self._marker(build)
        if marker is None or marker.get("key") != key:
            return not self._recently_failed(root, key)
        if self.max_age_seconds <= 0:
            return False
        built_at = marker.get("built_at_epoch")
        if not isinstance(built_at, (int, float)):
            return True
        if _now() - built_at < self.max_age_seconds:
            return False
        # A failed refresh keeps serving the existing env until the backoff ends.
        return not self._recently_failed(root, key)

    def _failure_path(self, root: Path, key: str) -> Path:
        return root / FAILURES_DIR_NAME / key

    def _recently_failed(self, root: Path, key: str) -> bool:
        try:
            failed_at = self._failure_path(root, key).stat().st_mtime
        except OSError:
            return False
        return _now() - failed_at < self.failure_backoff_seconds

    def _record_failure(self, root: Path, key: str, project: Path, detail: str) -> None:
        path = self._failure_path(root, key)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{project}\n{detail[-OUTPUT_TAIL_CHARS:]}\n")

    # ── build ────────────────────────────────────────────────────────────────

    def _run(self, args: list[str]) -> None:
        env = os.environ.copy()
        env["UV_CACHE_DIR"] = str(self.uv_cache_dir)
        env["UV_PYTHON_DOWNLOADS"] = "never"
        env["UV_LINK_MODE"] = "hardlink"
        env.pop("VIRTUAL_ENV", None)
        result = subprocess.run(
            args,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=self.build_timeout_seconds,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"{' '.join(args[:3])} exited {result.returncode}: {result.stdout.strip()}"
            )

    def build(self, root: Path, project: Path, key: str) -> bool:
        started = _now()
        stamp = f"{key}-{int(started * 1000)}"
        builds = root / BUILDS_DIR_NAME
        tmp = builds / f"{TMP_PREFIX}{stamp}"
        final = builds / stamp
        _remove(tmp)
        tmp.mkdir(parents=True)
        try:
            # Install from a private copy so a checkout updated mid-build cannot
            # produce an env whose contents differ from its key.
            staged = tmp / "src" / project.name
            shutil.copytree(project, staged, symlinks=True, ignore=_stage_ignore)
            if shims.tool_env_key(staged) != key:
                _remove(tmp)
                print(f"tool-envs: {project} changed during staging; retrying later", flush=True)
                return False
            venv = tmp / "venv"
            self._run(
                [self.uv, "venv", "--quiet", "--relocatable", "--python", self.python, str(venv)]
            )
            self._run(
                [
                    self.uv,
                    "pip",
                    "install",
                    "--quiet",
                    "--python",
                    str(venv / "bin" / "python"),
                    str(staged),
                ]
            )
            shutil.rmtree(tmp / "src")
            self.compile_bytecode(root, venv)
            finished = _now()
            marker = {
                "schema": shims.TOOL_ENV_SCHEMA,
                "key": key,
                "tool": project.name,
                "interpreter": shims.tool_env_interpreter_tag(),
                "built_at": _timestamp(finished),
                "built_at_epoch": finished,
            }
            (tmp / shims.TOOL_ENV_MARKER_NAME).write_text(json.dumps(marker, indent=2) + "\n")
            tmp.rename(final)
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
            _remove(tmp)
            self._record_failure(root, key, project, str(exc))
            print(f"tool-envs: failed to build {project.name} ({key}): {exc}", file=sys.stderr, flush=True)
            return False

        link = root / key
        tmp_link = root / f"{TMP_PREFIX}{key}"
        _remove(tmp_link)
        tmp_link.symlink_to(f"{BUILDS_DIR_NAME}/{stamp}")
        os.replace(tmp_link, link)
        _remove(self._failure_path(root, key))
        print(
            f"tool-envs: built {project.name} ({key}) in {finished - started:.1f}s",
            flush=True,
        )
        return True

    def compile_bytecode(self, root: Path, venv: Path) -> None:
        """Precompile site-packages and share identical .pyc files across envs.

        Sandboxes run with PYTHONDONTWRITEBYTECODE=1 against a read-only mount,
        so without this every call recompiles every module it imports.
        """
        for site_packages in sorted((venv / "lib").glob("python*/site-packages")):
            # Files that fail to compile (e.g. vendored py2 sources) are skipped,
            # matching uv's --compile-bytecode; they are never imported anyway.
            subprocess.run(
                [
                    str(venv / "bin" / "python"),
                    "-m",
                    "compileall",
                    "-q",
                    "-d",
                    PYC_COMPILE_PREFIX,
                    str(site_packages),
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=self.build_timeout_seconds,
                check=False,
            )
            self._dedupe_pyc(root / PYC_STORE_DIR_NAME, site_packages)

    @staticmethod
    def _dedupe_pyc(store: Path, site_packages: Path) -> None:
        for pyc in site_packages.rglob("*.pyc"):
            if pyc.is_symlink() or not pyc.is_file():
                continue
            digest = hashlib.sha256(pyc.read_bytes()).hexdigest()
            shared = store / digest[:2] / digest
            if shared.is_file():
                linked = pyc.with_name(f"{pyc.name}.link")
                os.link(shared, linked)
                os.replace(linked, pyc)
            else:
                shared.parent.mkdir(parents=True, exist_ok=True)
                os.link(pyc, shared)

    @staticmethod
    def _collect_pyc_store(store: Path) -> None:
        """Drop shared .pyc files no published env links to any more."""
        if not store.is_dir():
            return
        for shard in list(store.iterdir()):
            for shared in list(shard.iterdir()):
                if shared.stat().st_nlink <= 1:
                    _remove(shared)
            try:
                shard.rmdir()
            except OSError:
                pass

    # ── garbage collection ───────────────────────────────────────────────────

    def collect(self, root: Path, current: set[str]) -> bool:
        """Drop retired keys and unreferenced builds; return whether anything went."""
        now = _now()
        removed = False
        referenced: set[str] = set()
        for entry in list(root.iterdir()):
            if not entry.is_symlink():
                continue
            if entry.name.startswith(TMP_PREFIX):
                _remove(entry)
                continue
            if entry.name in current:
                os.utime(entry, follow_symlinks=False)
            elif now - entry.lstat().st_mtime >= self.retention_seconds:
                _remove(entry)
                removed = True
                continue
            referenced.add(Path(os.readlink(entry)).name)

        builds = root / BUILDS_DIR_NAME
        if builds.is_dir():
            for build in list(builds.iterdir()):
                if build.name.startswith(TMP_PREFIX):
                    if now - build.stat().st_mtime >= self.build_timeout_seconds * 2:
                        _remove(build)
                    continue
                orphaned = build / ORPHANED_MARKER
                if build.name in referenced:
                    _remove(orphaned)
                    continue
                # Keep a replaced build for a grace period: a sandbox that
                # resolved it just before the swap may still be importing from it.
                if not orphaned.exists():
                    orphaned.touch()
                elif now - orphaned.stat().st_mtime >= self.grace_seconds:
                    _remove(build)
                    removed = True
        if removed:
            self._collect_pyc_store(root / PYC_STORE_DIR_NAME)

        failures = root / FAILURES_DIR_NAME
        if failures.is_dir():
            for failure in list(failures.iterdir()):
                if failure.name not in current:
                    _remove(failure)
        return removed

    # ── loop ─────────────────────────────────────────────────────────────────

    def sync_once(self) -> int:
        built = 0
        removed = False
        for visibility in VISIBILITIES:
            root = self.envs_root(visibility)
            if not root.is_dir() or not os.access(root, os.W_OK):
                continue
            current: set[str] = set()
            for project in self.tool_projects(visibility):
                try:
                    key = shims.tool_env_key(project)
                except OSError as exc:
                    print(f"tool-envs: failed to hash {project}: {exc}", file=sys.stderr, flush=True)
                    continue
                current.add(key)
                if self.needs_build(root, key) and self.build(root, project, key):
                    built += 1
            removed = self.collect(root, current) or removed
        if removed:
            # Envs hardlink their files out of the uv cache; pruning only drops
            # cache entries, never files a published env still links to.
            try:
                self._run([self.uv, "cache", "prune", "--quiet"])
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
                print(f"tool-envs: uv cache prune failed: {exc}", file=sys.stderr, flush=True)
        return built

    def ready(self) -> bool:
        """repo-cache-sync creates the builder-owned dirs; it runs as root."""
        try:
            self.uv_cache_dir.mkdir(parents=True, exist_ok=True)
        except OSError:
            return False
        return os.access(self.uv_cache_dir, os.W_OK)

    @contextmanager
    def _lock(self):
        with open(self.uv_cache_dir / ".tool-env-builder.lock", "a") as lock_file:
            fcntl.flock(lock_file, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock_file, fcntl.LOCK_UN)

    def run_forever(self) -> int:
        os.umask(0o022)
        if not self.sources:
            print("tool-envs: no tool sources configured", file=sys.stderr, flush=True)
            return 0
        while True:
            try:
                if self.ready():
                    with self._lock():
                        self.sync_once()
                else:
                    print("tool-envs: waiting for repo-cache to prepare tool env dirs", flush=True)
            except Exception as exc:  # keep serving existing envs on any bug
                print(f"tool-envs: sync failed: {exc}", file=sys.stderr, flush=True)
            time.sleep(self.interval_seconds)


def main() -> int:
    builder = ToolEnvBuilder.from_env()
    if "--once" in sys.argv[1:]:
        if not builder.ready():
            print(f"tool-envs: {builder.uv_cache_dir} is not writable", file=sys.stderr)
            return 1
        with builder._lock():
            builder.sync_once()
        return 0
    return builder.run_forever()


if __name__ == "__main__":
    raise SystemExit(main())
