from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import client as pond_client
from client import PondClient


class _Result:
    def __init__(self, returncode: int = 0, stdout: str = "ok", stderr: str = ""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def _capture(monkeypatch, result: _Result | Exception) -> list[list[str]]:
    calls: list[list[str]] = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        if isinstance(result, Exception):
            raise result
        return result

    monkeypatch.setattr(pond_client.subprocess, "run", fake_run)
    return calls


def test_search_builds_fts_query_with_filters(monkeypatch) -> None:
    calls = _capture(monkeypatch, _Result(stdout="hits"))
    out = PondClient().search(
        "billing tests failing",
        limit=5,
        project="reth",
        from_date="2026-06-01",
        sort_by="recency",
    )
    assert out == "hits"
    assert calls == [
        [
            "pond",
            "search",
            "billing tests failing",
            "--mode",
            "fts",
            "--limit",
            "5",
            "--sort-by",
            "recency",
            "--project",
            "reth",
            "--from-date",
            "2026-06-01",
        ]
    ]


def test_get_session_pages_from_end(monkeypatch) -> None:
    calls = _capture(monkeypatch, _Result(stdout="transcript"))
    PondClient().get_session("abc", limit=50, from_end=True)
    assert calls[0][:5] == ["pond", "get", "--session-id", "abc", "--session-limit"]
    assert "--session-from" in calls[0] and "end" in calls[0]


def test_get_message_includes_context(monkeypatch) -> None:
    calls = _capture(monkeypatch, _Result())
    PondClient().get_message("msg-1", context=5)
    assert "--message-context-before" in calls[0]
    assert calls[0][calls[0].index("--message-context-before") + 1] == "5"


def test_nonzero_exit_surfaces_stderr(monkeypatch) -> None:
    _capture(monkeypatch, _Result(returncode=2, stdout="", stderr="store unreachable"))
    with pytest.raises(RuntimeError, match="store unreachable"):
        PondClient().status()


def test_missing_binary_names_the_cause(monkeypatch) -> None:
    _capture(monkeypatch, FileNotFoundError())
    with pytest.raises(RuntimeError, match="pond binary not found"):
        PondClient().status()


def test_timeout_is_reported(monkeypatch) -> None:
    _capture(monkeypatch, subprocess.TimeoutExpired(cmd="pond", timeout=120.0))
    with pytest.raises(RuntimeError, match="timed out"):
        PondClient().search("anything")
