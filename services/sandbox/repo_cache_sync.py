#!/usr/bin/env python3
"""Keep configured GitHub repositories synced into the Centaur repo cache.

With ``--serve`` this also serves read-only git clones of the cache to
sandboxes. Each request must carry the principal API JWT that iron-proxy
injects; its ``repo_cache`` claim (``none``, ``public``, or ``all``) decides
which repositories the caller may clone.
"""

from __future__ import annotations

import base64
from datetime import datetime, timezone
import glob
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import parse_qs, urlsplit

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
        sync_interval_seconds: float,
        github_token_file: Path,
    ) -> None:
        self.cache_dir = cache_dir
        self.repositories = repositories
        self.repository_refs = repository_refs
        self.repository_visibilities = repository_visibilities
        self.sync_interval_seconds = sync_interval_seconds
        self.github_token_file = github_token_file
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
            sync_interval_seconds=sync_interval_seconds,
            github_token_file=Path(
                os.environ.get("GITHUB_TOKEN_FILE", "/github-token/token")
            ),
        )

    def _git_env(self) -> dict[str, str]:
        env = os.environ.copy()
        env["GIT_TERMINAL_PROMPT"] = "0"
        # The env is built once per process, so the token check lives in the
        # script: a token Secret that is filled or rotated after start is used
        # on the next fetch without restarting repo-cache.
        token_file = shlex.quote(str(self.github_token_file))
        askpass = Path("/tmp/git-askpass")
        askpass.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            "  *Username*) printf '%s\\n' x-access-token ;;\n"
            f"  *Password*) if [ -s {token_file} ]; then cat {token_file}; "
            "else printf '\\n'; fi ;;\n"
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
                        "--force",
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
        repo_url = f"https://github.com/{repo}.git"
        target = self.repository_target(repo)
        tmp = target.with_name(f"{target.name}.tmp")
        target.parent.mkdir(parents=True, exist_ok=True)
        self.migrate_existing_checkout(repo, target)

        if self._git_ok(target, "rev-parse", "--git-dir"):
            print(f"Updating {repo}", flush=True)
            self._git_ok(target, "config", "gc.auto", "0")
            if not self._git_ok(target, "remote", "set-url", "origin", repo_url):
                self._run_git(
                    ["-C", str(target), "remote", "add", "origin", repo_url],
                    f"set origin for {repo}",
                )
            self._run_git(
                [
                    "-C",
                    str(target),
                    "-c",
                    "gc.auto=0",
                    "fetch",
                    "--force",
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
        _remove_path(target)
        self._run_git(["clone", "--quiet", repo_url, str(tmp)], f"clone {repo}")
        self._git_ok(tmp, "config", "gc.auto", "0")
        self._run_git(
            [
                "-C",
                str(tmp),
                "-c",
                "gc.auto=0",
                "fetch",
                "--force",
                "--prune",
                "--tags",
                "origin",
            ],
            f"fetch {repo}",
        )
        self._git_ok(tmp, "remote", "set-head", "origin", "-a")
        self.checkout_repo(repo, tmp)
        self._run_git(["-C", str(tmp), "clean", "-fd"], f"clean {repo}")
        tmp.replace(target)
        self.remove_stale_visibility_target(repo)
        self.update_legacy_link(repo, target)

    def repository_fingerprint(self) -> str:
        refs = " ".join(f"{repo}={ref}" for repo, ref in self.repository_refs.items())
        visibilities = " ".join(
            f"{repo}={self.repository_visibilities.get(repo, 'private')}"
            for repo in self.repositories
        )
        return (
            f"repositories={' '.join(self.repositories)}\n"
            f"repository_refs={refs}\n"
            f"repository_visibilities={visibilities}\n"
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


JWT_CLOCK_SKEW_SECONDS = 30
REPO_CACHE_ACCESS_LEVELS = {"none", "public", "all"}
# Upload-pack requests carry wants/haves only; packs flow the other way.
MAX_REQUEST_BODY_BYTES = 32 * 1024 * 1024
GIT_ROUTE = re.compile(
    r"^/(?P<repo>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+?)(?:\.git)?"
    r"/(?P<endpoint>info/refs|git-upload-pack)$"
)
# Serve only the smart upload-pack protocol: no pushes and no dumb file reads.
HTTP_BACKEND_GIT_CONFIG = {
    "safe.directory": "*",
    "http.receivepack": "false",
    "http.getanyfile": "false",
    "http.uploadpack": "true",
    "uploadpack.allowFilter": "true",
}


def _b64url_decode(value: str) -> bytes:
    padded = value + "=" * (-len(value) % 4)
    return base64.b64decode(padded, altchars=b"-_", validate=True)


def verify_api_jwt(
    token: str, *, secret: bytes, audience: str, issuer: str, now: float
) -> dict[str, object] | None:
    """Return the claims of a valid HS256 principal API JWT, else None.

    Mirrors api-rs's validation: exact algorithm, signature, audience, issuer,
    expiry, not-before, issued-at, and a non-empty subject.
    """
    parts = token.split(".")
    if len(parts) != 3:
        return None
    try:
        header = json.loads(_b64url_decode(parts[0]))
        signature = _b64url_decode(parts[2])
        claims = json.loads(_b64url_decode(parts[1]))
    except (ValueError, TypeError):
        return None
    if not isinstance(header, dict) or header.get("alg") != "HS256":
        return None
    expected = hmac.new(
        secret, f"{parts[0]}.{parts[1]}".encode("ascii"), hashlib.sha256
    ).digest()
    if not hmac.compare_digest(signature, expected) or not isinstance(claims, dict):
        return None

    def number(name: str) -> float | None:
        value = claims.get(name)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return None
        return float(value)

    exp, iat = number("exp"), number("iat")
    if exp is None or exp <= now - JWT_CLOCK_SKEW_SECONDS:
        return None
    if iat is None or iat > now + JWT_CLOCK_SKEW_SECONDS:
        return None
    if "nbf" in claims:
        nbf = number("nbf")
        if nbf is None or nbf > now + JWT_CLOCK_SKEW_SECONDS:
            return None
    aud = claims.get("aud")
    if not (aud == audience or (isinstance(aud, list) and audience in aud)):
        return None
    if claims.get("iss") != issuer:
        return None
    sub = claims.get("sub")
    if not isinstance(sub, str) or not sub.strip():
        return None
    return claims


def repo_cache_access(claims: dict[str, object]) -> str:
    access = claims.get("repo_cache")
    return access if access in REPO_CACHE_ACCESS_LEVELS else "none"


class RepoCacheServer:
    def __init__(
        self, *, sync: RepoCacheSync, secret: bytes, audience: str, issuer: str
    ) -> None:
        self.sync = sync
        self.secret = secret
        self.audience = audience
        self.issuer = issuer

    @classmethod
    def from_env(cls, sync: RepoCacheSync) -> RepoCacheServer:
        secret = os.environ.get("CENTAUR_JWT_SIGNING_SECRET", "")
        if not secret.strip():
            raise SystemExit(
                "CENTAUR_JWT_SIGNING_SECRET is required to serve repo-cache"
            )
        return cls(
            sync=sync,
            secret=secret.encode(),
            audience=os.environ.get("CENTAUR_API_JWT_AUDIENCE", "").strip()
            or "centaur-api",
            issuer=os.environ.get("CENTAUR_API_JWT_ISSUER", "").strip()
            or "centaur-console",
        )

    def claims(self, authorization: str | None) -> dict[str, object] | None:
        scheme, _, token = (authorization or "").partition(" ")
        if scheme.lower() != "bearer" or not token.strip():
            return None
        return verify_api_jwt(
            token.strip(),
            secret=self.secret,
            audience=self.audience,
            issuer=self.issuer,
            now=time.time(),
        )

    def accessible_repositories(self, access: str) -> list[str]:
        if access not in {"public", "all"}:
            return []
        return [
            repo
            for repo in self.sync.repositories
            if (access == "all" or self.sync.repository_visibility(repo) == "public")
            and (self.sync.repository_target(repo) / ".git").is_dir()
        ]

    def repository_path(self, repo: str, access: str) -> Path | None:
        if repo not in self.accessible_repositories(access):
            return None
        return self.sync.repository_target(repo)

    def serve_forever(self, port: int) -> int:
        server = ThreadingHTTPServer(("", port), _handler_class(self))
        server.daemon_threads = True
        print(f"repo-cache serving on :{port}", flush=True)
        server.serve_forever()
        return 0


def _read_chunked(stream, limit: int) -> bytes:
    body = bytearray()
    while True:
        size_line = stream.readline(1024).split(b";", 1)[0].strip()
        size = int(size_line, 16)
        if size == 0:
            while stream.readline(1024).strip():
                pass
            return bytes(body)
        if len(body) + size > limit:
            raise ValueError("request body too large")
        body += stream.read(size)
        stream.readline(1024)


def _handler_class(server: RepoCacheServer) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "centaur-repo-cache"

        def log_message(self, format: str, *args: object) -> None:  # noqa: A002
            pass

        def _log(self, status: int, subject: object = None, repo: str = "") -> None:
            print(
                json.dumps(
                    {
                        "event": "repo_cache_request",
                        "method": self.command,
                        "path": urlsplit(self.path).path,
                        "status": status,
                        "sub": subject,
                        "repo": repo,
                    }
                ),
                flush=True,
            )

        def _reply(
            self, status: int, body: bytes, content_type: str = "text/plain"
        ) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802
            self._dispatch()

        def do_POST(self) -> None:  # noqa: N802
            self._dispatch()

        def _dispatch(self) -> None:
            url = urlsplit(self.path)
            if url.path == "/healthz" and self.command == "GET":
                self._reply(200, b"ok\n")
                return
            claims = server.claims(self.headers.get("Authorization"))
            if claims is None:
                # 403 rather than 401 so git never prompts for credentials.
                self._log(403)
                self._reply(
                    403, b"repo-cache access requires a valid principal token\n"
                )
                return
            subject = claims.get("sub")
            access = repo_cache_access(claims)
            if url.path == "/repos" and self.command == "GET":
                body = json.dumps(
                    {"repositories": server.accessible_repositories(access)}
                ).encode()
                self._log(200, subject)
                self._reply(200, body, "application/json")
                return
            match = GIT_ROUTE.match(url.path)
            endpoint = match["endpoint"] if match else ""
            allowed = (
                endpoint == "info/refs"
                and self.command == "GET"
                and parse_qs(url.query).get("service") == ["git-upload-pack"]
            ) or (endpoint == "git-upload-pack" and self.command == "POST")
            repo = match["repo"] if match else ""
            repo_path = server.repository_path(repo, access) if allowed else None
            if repo_path is None:
                self._log(404, subject, repo)
                self._reply(404, b"not found\n")
                return
            try:
                body = self._request_body()
            except ValueError:
                self._log(400, subject, repo)
                self._reply(400, b"invalid request body\n")
                return
            status = self._run_http_backend(repo_path, endpoint, url.query, body)
            self._log(status, subject, repo)

        def _request_body(self) -> bytes:
            if self.command != "POST":
                return b""
            if "chunked" in self.headers.get("Transfer-Encoding", "").lower():
                return _read_chunked(self.rfile, MAX_REQUEST_BODY_BYTES)
            length = int(self.headers.get("Content-Length") or 0)
            if length < 0 or length > MAX_REQUEST_BODY_BYTES:
                raise ValueError("request body too large")
            return self.rfile.read(length)

        def _run_http_backend(
            self, repo_path: Path, endpoint: str, query: str, body: bytes
        ) -> int:
            env = {
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "HOME": tempfile.gettempdir(),
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_PROJECT_ROOT": str(repo_path.parent),
                "GIT_HTTP_EXPORT_ALL": "1",
                "PATH_INFO": f"/{repo_path.name}/{endpoint}",
                "REQUEST_METHOD": self.command,
                "QUERY_STRING": query,
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(len(body)),
                "REMOTE_ADDR": self.client_address[0],
                "GIT_CONFIG_COUNT": str(len(HTTP_BACKEND_GIT_CONFIG)),
            }
            for index, (key, value) in enumerate(HTTP_BACKEND_GIT_CONFIG.items()):
                env[f"GIT_CONFIG_KEY_{index}"] = key
                env[f"GIT_CONFIG_VALUE_{index}"] = value
            if protocol := self.headers.get("Git-Protocol"):
                env["GIT_PROTOCOL"] = protocol
            if encoding := self.headers.get("Content-Encoding"):
                env["HTTP_CONTENT_ENCODING"] = encoding
            with tempfile.TemporaryFile() as stdin:
                stdin.write(body)
                stdin.seek(0)
                process = subprocess.Popen(
                    ["git", "http-backend"],
                    env=env,
                    stdin=stdin,
                    stdout=subprocess.PIPE,
                )
            assert process.stdout is not None
            try:
                status, headers, complete = 200, [], False
                while line := process.stdout.readline():
                    line = line.rstrip(b"\r\n")
                    if not line:
                        complete = True
                        break
                    name, _, value = line.decode("latin-1").partition(":")
                    if name.lower() == "status":
                        status = int(value.strip().split()[0])
                    else:
                        headers.append((name, value.strip()))
                if not complete:
                    self._reply(502, b"git http-backend failed\n")
                    return 502
                self.send_response(status)
                for name, value in headers:
                    self.send_header(name, value)
                self.end_headers()
                shutil.copyfileobj(process.stdout, self.wfile)
            finally:
                process.stdout.close()
                process.wait()
            return status

    return Handler


def main() -> int:
    sync = RepoCacheSync.from_env()
    if "--check-ready" in sys.argv[1:]:
        return sync.check_ready()
    if "--serve" in sys.argv[1:]:
        port = int(os.environ.get("REPO_CACHE_SERVER_PORT", "8080"))
        return RepoCacheServer.from_env(sync).serve_forever(port)
    return sync.run_forever()


if __name__ == "__main__":
    raise SystemExit(main())
