"""Tests for the workflows-side Linear read-only GraphQL helpers.

This mirrors tools/productivity/linear/test_readonly.py. The two clients are
independent copies of the same code, and the `term:` fix landed in only one of
them -- so the invariant is pinned on both sides now, or they drift again.
"""

from __future__ import annotations

from typing import Any

from workflows.linear.readonly import LinearReadonlyClient


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
    # `searchIssues` names its search string `term` and requires it. `query` was
    # the argument of `issueSearch`, the endpoint Linear deprecated in favour of
    # this one; passing it here fails GraphQL validation, which Linear returns as
    # an HTTP 400.
    client = RecordingReadonlyClient()

    result = client.search_issues("auth", limit=1)

    assert result == [{"identifier": "ENG-1", "title": "Search result"}]
    assert "searchIssues(term: $term" in client.calls[0]["query"]
    assert "query:" not in client.calls[0]["query"]
    assert client.calls[0]["variables"] == {"term": "auth", "first": 1, "after": None}
