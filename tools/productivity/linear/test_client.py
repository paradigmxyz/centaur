"""Tests for the Linear tool client's mutation result handling.

Run from this directory: uv run --no-project --with pytest pytest test_client.py
"""

from __future__ import annotations

import importlib.util
import sys
import types
from pathlib import Path
from typing import Any

# client.py inherits from the packaged readonly client. The mutation logic under
# test never touches readonly behavior, so stub the base class before loading the
# module as a standalone file.
if "readonly" not in sys.modules:
    readonly_mod = types.ModuleType("readonly")

    class LinearReadonlyClient:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            pass

        def _query(self, query: str, variables: dict | None = None) -> dict:
            raise NotImplementedError

    readonly_mod.LinearReadonlyClient = LinearReadonlyClient
    sys.modules["readonly"] = readonly_mod

spec = importlib.util.spec_from_file_location(
    "linear_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
LinearClient = module.LinearClient


class RecordingLinearClient(LinearClient):
    """Returns canned mutation payloads keyed by substring, records calls."""

    def __init__(
        self,
        responses: dict[str, Any],
        *,
        teams: list[dict[str, Any]] | None = None,
        projects: list[dict[str, Any]] | None = None,
    ) -> None:
        self.responses = responses
        self.calls: list[dict[str, Any]] = []
        self._teams = teams or []
        self._projects = projects or []

    def teams(self, limit: int = 50) -> list[dict[str, Any]]:
        return self._teams[:limit]

    def projects(self, limit: int = 50) -> list[dict[str, Any]]:
        return self._projects[:limit]

    def _query(self, query: str, variables: dict | None = None) -> dict:
        self.calls.append({"query": query, "variables": variables})
        for key, payload in self.responses.items():
            if key in query:
                return {key: payload}
        raise AssertionError(f"unexpected query: {query}")


def test_create_issue_merges_success_into_issue_fields():
    client = RecordingLinearClient(
        {
            "issueCreate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1", "title": "Test"},
            }
        }
    )

    created = client.create_issue("Test", team_id="team-1", priority=2)

    assert created["identifier"] == "ENG-1"
    assert created["success"] is True
    assert client.calls[0]["variables"]["input"] == {
        "title": "Test",
        "teamId": "team-1",
        "priority": 2,
    }


def test_create_project_creates_in_resolved_team():
    client = RecordingLinearClient(
        {
            "projectCreate": {
                "success": True,
                "project": {"id": "project-1", "name": "Upshift", "url": "https://linear/Upshift"},
            }
        },
        teams=[{"id": "team-1", "key": "INT", "name": "Integrations"}],
    )

    result = client.create_project("Upshift", team_key="INT", description="Vault integration")

    assert result["created"] is True
    assert result["reused"] is False
    assert result["team"]["key"] == "INT"
    assert client.calls[0]["variables"]["input"] == {
        "name": "Upshift",
        "teamIds": ["team-1"],
        "description": "Vault integration",
    }


def test_create_project_reuses_exact_project_without_mutation():
    existing = {
        "id": "project-1",
        "name": "Upshift",
        "url": "https://linear/Upshift",
        "teams": {"nodes": [{"id": "team-1", "key": "INT"}]},
    }
    client = RecordingLinearClient(
        {},
        teams=[{"id": "team-1", "key": "INT", "name": "Integrations"}],
        projects=[existing],
    )

    result = client.create_project("upshift", team_key="int")

    assert result["created"] is False
    assert result["reused"] is True
    assert result["id"] == "project-1"
    assert client.calls == []


def test_create_project_rejects_missing_team_and_duplicate_projects():
    missing_team = RecordingLinearClient({})
    try:
        missing_team.create_project("Upshift", team_key="INT")
    except ValueError as exc:
        assert "found 0" in str(exc)
    else:
        raise AssertionError("missing team should fail")

    duplicate = {
        "name": "Upshift",
        "teams": {"nodes": [{"id": "team-1", "key": "INT"}]},
    }
    ambiguous = RecordingLinearClient(
        {},
        teams=[{"id": "team-1", "key": "INT"}],
        projects=[{"id": "project-1", **duplicate}, {"id": "project-2", **duplicate}],
    )
    try:
        ambiguous.create_project("Upshift", team_key="INT")
    except ValueError as exc:
        assert "Multiple projects" in str(exc)
    else:
        raise AssertionError("duplicate projects should fail")
    assert ambiguous.calls == []


def test_update_issue_merges_success_into_issue_fields():
    client = RecordingLinearClient(
        {
            "issueUpdate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1", "title": "Renamed"},
            }
        }
    )

    updated = client.update_issue("ENG-1", title="Renamed")

    assert updated["title"] == "Renamed"
    assert updated["success"] is True


def test_add_comment_merges_success_into_comment_fields():
    client = RecordingLinearClient(
        {"commentCreate": {"success": True, "comment": {"id": "comment-1", "body": "hi"}}}
    )

    comment = client.add_comment("ENG-1", "hi")

    assert comment["id"] == "comment-1"
    assert comment["success"] is True


def test_mutations_surface_failure():
    client = RecordingLinearClient(
        {
            "issueCreate": {"success": False, "issue": None},
            "issueUpdate": {"success": False, "issue": None},
            "commentCreate": {"success": False, "comment": None},
        }
    )

    # Callers (e.g. workflow helpers) key on result["success"] is False.
    assert client.create_issue("Test", team_id="team-1") == {"success": False}
    assert client.update_issue("ENG-1", title="New title") == {"success": False}
    assert client.add_comment("ENG-1", "hello") == {"success": False}
