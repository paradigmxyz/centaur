from __future__ import annotations

import pytest


class FakeResponse:
    def __init__(self, payload: dict):
        self._payload = payload

    def raise_for_status(self) -> None:
        return None

    def json(self) -> dict:
        return self._payload


class FakeHttpClient:
    def __init__(self, payloads: list[dict]):
        self.payloads = list(payloads)
        self.requests: list[dict] = []

    def post(self, path: str, *, json: dict) -> FakeResponse:
        self.requests.append({"path": path, "json": json})
        return FakeResponse(self.payloads.pop(0))


def test_linear_graphql_client_raises_api_errors():
    from api.integrations.linear import LinearGraphQLClient

    client = LinearGraphQLClient(
        api_key="lin-test",
        http_client=FakeHttpClient(
            [{"errors": [{"message": "Nope"}]}],
        ),
    )

    with pytest.raises(RuntimeError, match="Linear API error: Nope"):
        client._query("query Test { viewer { id } }")


def test_linear_readonly_client_paginates_connections():
    from api.integrations.linear import LinearReadonlyClient

    http = FakeHttpClient(
        [
            {
                "data": {
                    "teams": {
                        "nodes": [
                            {"id": "team-1", "key": "ENG"},
                            {"id": "team-2", "key": "DES"},
                        ],
                        "pageInfo": {"hasNextPage": True, "endCursor": "cursor-2"},
                    }
                }
            },
            {
                "data": {
                    "teams": {
                        "nodes": [{"id": "team-3", "key": "OPS"}],
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                    }
                }
            },
        ]
    )
    client = LinearReadonlyClient(api_key="lin-test", http_client=http)

    result = client.teams(limit=3)

    assert [team["id"] for team in result] == ["team-1", "team-2", "team-3"]
    assert http.requests[0]["json"]["variables"] == {"first": 3, "after": None}
    assert http.requests[1]["json"]["variables"] == {"first": 1, "after": "cursor-2"}


def test_linear_readonly_client_builds_issue_filters():
    from api.integrations.linear import LinearReadonlyClient

    http = FakeHttpClient(
        [
            {
                "data": {
                    "issues": {
                        "nodes": [],
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                    }
                }
            }
        ]
    )
    client = LinearReadonlyClient(api_key="lin-test", http_client=http)

    client.issues(team_key='EN"G', assignee="Ada", state="In Progress", limit=1)

    query = http.requests[0]["json"]["query"]
    assert 'team: { key: { eq: "EN\\"G" } }' in query
    assert 'assignee: { name: { containsIgnoreCase: "Ada" } }' in query
    assert 'state: { name: { containsIgnoreCase: "In Progress" } }' in query


def test_linear_tool_client_keeps_mutations_and_inherits_readonly_methods():
    from tools.productivity.linear.client import LinearClient

    assert "projects" in LinearClient.__dict__
    assert "search_issues" in LinearClient.__dict__

    class FakeLinearClient(LinearClient):
        def __init__(self):
            self.calls = []

        def _query(self, query: str, variables: dict | None = None) -> dict:
            self.calls.append({"query": query, "variables": variables})
            if "query Projects" in query:
                return {
                    "projects": {
                        "nodes": [{"id": "project-1", "name": "Roadmap"}],
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                    }
                }
            return {
                "issueCreate": {
                    "issue": {
                        "id": "issue-1",
                        "identifier": "ENG-1",
                        "title": "Test",
                    }
                }
            }

    client = FakeLinearClient()
    projects = client.projects(limit=1)
    created = client.create_issue("Test", team_id="team-1", priority=2)

    assert projects == [{"id": "project-1", "name": "Roadmap"}]
    assert created["identifier"] == "ENG-1"
    assert client.calls[1]["variables"]["input"] == {
        "title": "Test",
        "teamId": "team-1",
        "priority": 2,
    }
