from __future__ import annotations

import gzip
import importlib.util
import subprocess
import sys
import zlib
from pathlib import Path
from types import ModuleType


def _load_repo_cache_server() -> ModuleType:
    path = (
        Path(__file__).resolve().parents[3]
        / "services"
        / "sandbox"
        / "repo-cache-server.py"
    )
    spec = importlib.util.spec_from_file_location("repo_cache_server", path)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _git(*args: str, cwd: Path | None = None) -> None:
    subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def test_parse_repo_from_path_accepts_git_http_paths() -> None:
    server = _load_repo_cache_server()

    assert server._parse_repo_from_path("/repos/github.com/org/repo.git/info/refs") == (
        "org/repo",
        "/github.com/org/repo.git/info/refs",
    )
    assert server._parse_repo_from_path(
        "/repos/github.com/org/repo.git/git-upload-pack"
    ) == (
        "org/repo",
        "/github.com/org/repo.git/git-upload-pack",
    )


def test_parse_repo_from_path_rejects_invalid_paths() -> None:
    server = _load_repo_cache_server()

    assert server._parse_repo_from_path("/healthz") is None
    assert server._parse_repo_from_path("/repos/github.com/org.git/info/refs") is None
    assert (
        server._parse_repo_from_path("/repos/github.com/../repo.git/info/refs") is None
    )


def test_is_receive_pack_detects_push_requests() -> None:
    server = _load_repo_cache_server()

    assert server._is_receive_pack(
        "/repos/github.com/org/repo.git/git-receive-pack", ""
    )
    assert server._is_receive_pack(
        "/repos/github.com/org/repo.git/info/refs", "service=git-receive-pack"
    )
    assert not server._is_receive_pack(
        "/repos/github.com/org/repo.git/info/refs", "service=git-upload-pack"
    )


def test_decode_request_body_accepts_git_http_content_encoding() -> None:
    server = _load_repo_cache_server()
    body = b"0032want abcdef\n0000"

    assert server._decode_request_body(gzip.compress(body), "gzip") == body
    assert server._decode_request_body(zlib.compress(body), "deflate") == body
    assert server._decode_request_body(body, "identity") == body
    assert server._decode_request_body(body, "") == body


def test_decode_request_body_rejects_unknown_content_encoding() -> None:
    server = _load_repo_cache_server()

    try:
        server._decode_request_body(b"body", "br")
    except ValueError as exc:
        assert "unsupported content encoding: br" in str(exc)
    else:
        raise AssertionError("expected unsupported encoding to raise")


def test_ensure_mirror_clones_bare_repo_from_upstream(tmp_path, monkeypatch) -> None:
    server = _load_repo_cache_server()
    origin_root = tmp_path / "origin"
    bare = origin_root / "org" / "repo.git"
    bare.parent.mkdir(parents=True)
    _git("init", "--bare", str(bare))

    work = tmp_path / "work"
    _git("init", str(work))
    _git("config", "user.email", "agent@example.com", cwd=work)
    _git("config", "user.name", "Agent", cwd=work)
    (work / "README.md").write_text("hello\n")
    _git("add", "README.md", cwd=work)
    _git("commit", "-m", "initial", cwd=work)
    _git("branch", "-M", "main", cwd=work)
    _git("remote", "add", "origin", str(bare), cwd=work)
    _git("push", "origin", "main", cwd=work)

    monkeypatch.setattr(server, "CACHE_ROOT", tmp_path / "cache" / "mirrors")
    monkeypatch.setattr(server, "LOCK_ROOT", tmp_path / "cache" / "locks")
    monkeypatch.setattr(server, "UPSTREAM_BASE_URL", origin_root.as_uri())
    monkeypatch.setattr(server, "FETCH_INTERVAL_SECONDS", 3600)

    mirror = server.ensure_mirror("org/repo")

    assert mirror == tmp_path / "cache" / "mirrors" / "github.com" / "org" / "repo.git"
    result = subprocess.run(
        ["git", "-C", str(mirror), "rev-parse", "--is-bare-repository"],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.stdout.strip() == "true"
