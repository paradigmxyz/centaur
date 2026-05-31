#!/usr/bin/env python3
from __future__ import annotations

import fcntl
import gzip
import os
import re
import shutil
import subprocess
import tempfile
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

CACHE_ROOT = Path(os.getenv("REPO_CACHE_ROOT", "/cache/mirrors"))
LOCK_ROOT = Path(os.getenv("REPO_CACHE_LOCK_ROOT", "/cache/locks"))
UPSTREAM_BASE_URL = os.getenv(
    "REPO_CACHE_UPSTREAM_BASE_URL", "https://github.com"
).rstrip("/")
FETCH_INTERVAL_SECONDS = int(
    os.getenv("REPO_CACHE_FETCH_INTERVAL_SECONDS")
    or os.getenv("SYNC_INTERVAL_SECONDS")
    or "300"
)
GITHUB_TOKEN_FILE = os.getenv("GITHUB_TOKEN_FILE", "/github-token/token")
REPOSITORIES = [repo for repo in os.getenv("REPOSITORIES", "").split() if repo]
PORT = int(os.getenv("REPO_CACHE_PORT", "8080"))

_REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def _git() -> str:
    return os.getenv("REPO_CACHE_GIT", "/usr/bin/git")


def _git_http_backend() -> str:
    configured = os.getenv("GIT_HTTP_BACKEND")
    if configured:
        return configured
    exec_path = subprocess.check_output([_git(), "--exec-path"], text=True).strip()
    return str(Path(exec_path) / "git-http-backend")


def _run(args: list[str], *, cwd: Path | None = None, timeout: int = 300) -> None:
    env = os.environ.copy()
    env.setdefault("GIT_TERMINAL_PROMPT", "0")
    subprocess.run(args, cwd=cwd, env=env, check=True, timeout=timeout)


def _configure_askpass() -> None:
    token_path = Path(GITHUB_TOKEN_FILE)
    if not token_path.exists() or token_path.stat().st_size == 0:
        return
    askpass = Path(tempfile.gettempdir()) / "repo-cache-git-askpass"
    askpass.write_text(
        "#!/bin/sh\n"
        'case "$1" in\n'
        "  *Username*) printf '%s\\n' x-access-token ;;\n"
        f"  *Password*) cat {GITHUB_TOKEN_FILE!r} ;;\n"
        "  *) printf '\\n' ;;\n"
        "esac\n"
    )
    askpass.chmod(0o700)
    os.environ["GIT_ASKPASS"] = str(askpass)
    os.environ.setdefault("GIT_TERMINAL_PROMPT", "0")


def _repo_dir(repo: str) -> Path:
    return CACHE_ROOT / "github.com" / f"{repo}.git"


def _lock_path(repo: str) -> Path:
    return LOCK_ROOT / "github.com" / f"{repo}.lock"


def _validate_repo(repo: str) -> None:
    if not _REPO_RE.fullmatch(repo):
        raise ValueError(f"invalid GitHub repo path: {repo}")
    owner, name = repo.split("/", 1)
    if owner in {".", ".."} or name in {".", ".."}:
        raise ValueError(f"invalid GitHub repo path: {repo}")


def _upstream_url(repo: str) -> str:
    return f"{UPSTREAM_BASE_URL}/{repo}.git"


def _last_fetch_path(target: Path) -> Path:
    return target / "centaur-last-fetch"


def _is_fresh(target: Path) -> bool:
    marker = _last_fetch_path(target)
    if not marker.exists():
        return False
    return time.time() - marker.stat().st_mtime < FETCH_INTERVAL_SECONDS


def _mark_fetched(target: Path) -> None:
    marker = _last_fetch_path(target)
    marker.write_text(str(int(time.time())) + "\n")


def ensure_mirror(repo: str, *, force_fetch: bool = False) -> Path:
    _validate_repo(repo)
    target = _repo_dir(repo)
    lock_path = _lock_path(repo)
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    CACHE_ROOT.mkdir(parents=True, exist_ok=True)

    with lock_path.open("w") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        if target.exists() and (target / "HEAD").exists():
            if force_fetch or not _is_fresh(target):
                _run(
                    [
                        _git(),
                        "-C",
                        str(target),
                        "remote",
                        "set-url",
                        "origin",
                        _upstream_url(repo),
                    ]
                )
                _run([_git(), "-C", str(target), "remote", "update", "--prune"])
                _mark_fetched(target)
            return target

        if target.exists():
            shutil.rmtree(target)
        tmp = target.with_name(f"{target.name}.tmp.{os.getpid()}.{int(time.time())}")
        if tmp.exists():
            shutil.rmtree(tmp)
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            _run(
                [_git(), "clone", "--mirror", _upstream_url(repo), str(tmp)],
                timeout=600,
            )
            _mark_fetched(tmp)
            tmp.replace(target)
        except Exception:
            shutil.rmtree(tmp, ignore_errors=True)
            raise
        return target


def _parse_repo_from_path(path: str) -> tuple[str, str] | None:
    prefix = "/repos/github.com/"
    if not path.startswith(prefix):
        return None
    rest = path[len(prefix) :]
    marker = ".git"
    idx = rest.find(marker)
    if idx < 0:
        return None
    repo = rest[:idx]
    suffix = rest[idx + len(marker) :]
    if suffix and not suffix.startswith("/"):
        return None
    try:
        _validate_repo(repo)
    except ValueError:
        return None
    path_info = f"/github.com/{repo}.git{suffix}"
    return repo, path_info


def _is_receive_pack(path: str, query: str) -> bool:
    query_params = parse_qs(query)
    services = query_params.get("service", [])
    return "git-receive-pack" in services or path.endswith("/git-receive-pack")


def _decode_request_body(body: bytes, content_encoding: str | None) -> bytes:
    encodings = [
        encoding.strip().lower()
        for encoding in (content_encoding or "").split(",")
        if encoding.strip()
    ]
    for encoding in reversed(encodings):
        if encoding in {"identity"}:
            continue
        if encoding in {"gzip", "x-gzip"}:
            body = gzip.decompress(body)
            continue
        if encoding == "deflate":
            body = zlib.decompress(body)
            continue
        raise ValueError(f"unsupported content encoding: {encoding}")
    return body


def _prewarm_loop() -> None:
    while True:
        for repo in REPOSITORIES:
            try:
                ensure_mirror(repo)
            except Exception as exc:  # noqa: BLE001
                print(f"repo-cache prewarm failed for {repo}: {exc}", flush=True)
        time.sleep(FETCH_INTERVAL_SECONDS)


class RepoCacheHandler(BaseHTTPRequestHandler):
    server_version = "centaur-repo-cache/1.0"

    def do_GET(self) -> None:  # noqa: N802
        self._handle_request()

    def do_POST(self) -> None:  # noqa: N802
        self._handle_request()

    def log_message(self, fmt: str, *args: object) -> None:
        print(f"{self.address_string()} - {fmt % args}", flush=True)

    def _handle_request(self) -> None:
        split = urlsplit(self.path)
        if split.path == "/healthz":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok\n")
            return
        parsed = _parse_repo_from_path(split.path)
        if parsed is None:
            self.send_error(404, "unknown repository path")
            return
        if _is_receive_pack(split.path, split.query):
            self.send_error(403, "repo-cache is fetch-only")
            return

        repo, path_info = parsed
        try:
            ensure_mirror(repo)
        except Exception as exc:  # noqa: BLE001
            self.send_error(502, f"failed to mirror repository: {exc}")
            return

        content_length = int(self.headers.get("Content-Length") or "0")
        request_body = self.rfile.read(content_length) if content_length else b""
        try:
            request_body = _decode_request_body(
                request_body, self.headers.get("Content-Encoding")
            )
        except Exception as exc:  # noqa: BLE001
            self.send_error(415, f"unsupported request content encoding: {exc}")
            return
        env = os.environ.copy()
        env.update(
            {
                "GIT_PROJECT_ROOT": str(CACHE_ROOT),
                "GIT_HTTP_EXPORT_ALL": "1",
                "PATH_INFO": path_info,
                "QUERY_STRING": split.query,
                "REQUEST_METHOD": self.command,
                "REMOTE_ADDR": self.client_address[0],
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(len(request_body)),
            }
        )
        proc = subprocess.run(
            [_git_http_backend()],
            input=request_body,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            check=False,
        )
        if proc.stderr:
            print(proc.stderr.decode("utf-8", "replace"), flush=True)
        self._write_cgi_response(proc.stdout)

    def _write_cgi_response(self, payload: bytes) -> None:
        header_blob, sep, body = payload.partition(b"\r\n\r\n")
        if not sep:
            header_blob, sep, body = payload.partition(b"\n\n")
        if not sep:
            self.send_error(502, "invalid git-http-backend response")
            return

        status = 200
        headers: list[tuple[str, str]] = []
        for raw_line in header_blob.replace(b"\r\n", b"\n").split(b"\n"):
            if not raw_line:
                continue
            line = raw_line.decode("iso-8859-1")
            name, _, value = line.partition(":")
            if not _:
                continue
            name = name.strip()
            value = value.strip()
            if name.lower() == "status":
                try:
                    status = int(value.split()[0])
                except Exception:
                    status = 200
            else:
                headers.append((name, value))
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)


def main() -> None:
    _configure_askpass()
    if REPOSITORIES:
        thread = threading.Thread(target=_prewarm_loop, daemon=True)
        thread.start()
    server = ThreadingHTTPServer(("0.0.0.0", PORT), RepoCacheHandler)
    print(f"repo-cache listening on :{PORT}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
