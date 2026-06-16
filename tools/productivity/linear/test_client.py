"""Tests for the Linear tool client's mutation result handling.

Run from this directory: uv run --no-project --with pytest pytest test_client.py
"""

from __future__ import annotations

import importlib.util
import sys
import types
from pathlib import Path
from typing import Any

# client.py inherits from workflows.linear.readonly.LinearReadonlyClient, which
# lives in the workflows package and may not be importable in isolation. The
# mutation logic under test never touches readonly behavior, so stub the base
# class before loading the module.
if "workflows.linear.readonly" not in sys.modules:
    workflows_pkg = types.ModuleType("workflows")
    linear_pkg = types.ModuleType("workflows.linear")
    readonly_mod = types.ModuleType("workflows.linear.readonly")

    class LinearReadonlyClient:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            pass

        def _query(self, query: str, variables: dict | None = None) -> dict:
            raise NotImplementedError

    readonly_mod.LinearReadonlyClient = LinearReadonlyClient
    linear_pkg.readonly = readonly_mod
    workflows_pkg.linear = linear_pkg
    sys.modules["workflows"] = workflows_pkg
    sys.modules["workflows.linear"] = linear_pkg
    sys.modules["workflows.linear.readonly"] = readonly_mod

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
