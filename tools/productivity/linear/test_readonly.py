"""Tests for Linear read-only GraphQL helpers.

Run from this directory: uv run --no-project --with pytest --with httpx pytest test_readonly.py
"""

from __future__ import annotations

import sys
import types
from typing import Any

if "centaur_sdk" not in sys.modules:
    sdk_mod = types.ModuleType("centaur_sdk")
    sdk_mod.secret = lambda name, default="": default
    sys.modules["centaur_sdk"] = sdk_mod

from centaur_tool_linear.readonly import LinearReadonlyClient


class RecordingReadonlyClient(LinearReadonlyClient):
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    def _query(self, query: str, variables: dict[str, Any] | None = None) -> dict[str, Any]:
        self.calls.append({"query": query, "variables": variables})
        if "projectMilestones" in query:
            return {
                "projectMilestones": {
                    "nodes": [{"id": "milestone-1", "name": "Beta"}],
                    "pageInfo": {"hasNextPage": False, "endCursor": None},
                }
            }
        if "query Issues" in query:
            return {
                "issues": {
                    "nodes": [{"identifier": "ENG-1", "title": "Filtered"}],
                    "pageInfo": {"hasNextPage": False, "endCursor": None},
                }
            }
        return {
            "searchIssues": {
                "nodes": [{"identifier": "ENG-1", "title": "Search result"}],
                "pageInfo": {"hasNextPage": False, "endCursor": None},
            }
        }


def test_search_issues_uses_linear_term_argument():
    client = RecordingReadonlyClient()

    result = client.search_issues("auth", limit=1)

    assert result == [{"identifier": "ENG-1", "title": "Search result"}]
    assert "searchIssues(term: $term" in client.calls[0]["query"]
    assert "query:" not in client.calls[0]["query"]
    assert client.calls[0]["variables"] == {"term": "auth", "first": 1, "after": None}


def test_project_milestones_filters_by_project_id():
    client = RecordingReadonlyClient()

    result = client.project_milestones(project_id="project-1", limit=10)

    assert result == [{"id": "milestone-1", "name": "Beta"}]
    call = client.calls[0]
    assert "project: { id: { eq: $projectId } }" in call["query"]
    assert call["variables"] == {
        "projectId": "project-1",
        "first": 10,
        "after": None,
    }


def test_issues_filters_by_project_and_milestone_ids():
    client = RecordingReadonlyClient()

    result = client.issues(
        project_id="project-1",
        project_milestone_id="milestone-1",
        limit=10,
    )

    assert result == [{"identifier": "ENG-1", "title": "Filtered"}]
    query = client.calls[0]["query"]
    assert 'project: { id: { eq: "project-1" } }' in query
    assert 'projectMilestone: { id: { eq: "milestone-1" } }' in query
