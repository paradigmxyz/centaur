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
