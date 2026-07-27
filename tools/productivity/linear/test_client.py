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

    def __init__(self, responses: dict[str, Any]) -> None:
        self.responses = responses
        self.calls: list[dict[str, Any]] = []

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


def test_create_issue_sets_project_and_milestone():
    client = RecordingLinearClient(
        {
            "issueCreate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1"},
            }
        }
    )

    client.create_issue(
        "Test",
        team_id="team-1",
        project_id="project-1",
        project_milestone_id="milestone-1",
    )

    assert client.calls[0]["variables"]["input"] == {
        "title": "Test",
        "teamId": "team-1",
        "projectId": "project-1",
        "projectMilestoneId": "milestone-1",
    }


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


def test_update_issue_sets_project_milestone():
    client = RecordingLinearClient(
        {
            "issueUpdate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1"},
            }
        }
    )

    client.update_issue("ENG-1", project_milestone_id="milestone-1")

    assert client.calls[0]["variables"]["input"] == {"projectMilestoneId": "milestone-1"}


def test_update_issue_clears_project_milestone():
    client = RecordingLinearClient(
        {
            "issueUpdate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1"},
            }
        }
    )

    client.update_issue("ENG-1", clear_project_milestone=True)

    assert client.calls[0]["variables"]["input"] == {"projectMilestoneId": None}


def test_update_issue_rejects_set_and_clear_project_milestone():
    client = RecordingLinearClient({})

    try:
        client.update_issue(
            "ENG-1",
            project_milestone_id="milestone-1",
            clear_project_milestone=True,
        )
    except ValueError as exc:
        assert "mutually exclusive" in str(exc)
    else:
        raise AssertionError("expected ValueError")


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
