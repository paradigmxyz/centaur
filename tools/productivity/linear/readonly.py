from __future__ import annotations

import base64
import datetime as dt
import mimetypes
from typing import Any
from urllib.parse import urlparse

import httpx

try:
    from .graphql import UPLOADS_PREFIX, LinearGraphQLClient
except ImportError:  # pragma: no cover - supports file-based plugin loading
    from graphql import UPLOADS_PREFIX, LinearGraphQLClient


def _linear_string_literal(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _team_key_filter(team_key: str) -> str:
    return f"team: {{ key: {{ eq: {_linear_string_literal(team_key)} }} }}"


def _linear_datetime(value: dt.datetime | str | None) -> str | None:
    if value is None:
        return None
    if isinstance(value, dt.datetime):
        parsed = value
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=dt.UTC)
        return parsed.astimezone(dt.UTC).isoformat().replace("+00:00", "Z")
    return value


class LinearReadonlyClient(LinearGraphQLClient):
    """Read-only Linear API surface used by tools and ETL workflows."""

    def me(self) -> dict[str, Any]:
        """Get authenticated user info."""
        query = """
        query Me {
            viewer { id name email }
        }
        """
        return self._query(query).get("viewer", {})

    def teams(self, limit: int = 50) -> list[dict[str, Any]]:
        """List teams."""
        query = """
        query Teams($first: Int!, $after: String) {
            teams(first: $first, after: $after) {
                nodes { id name key description }
                pageInfo { hasNextPage endCursor }
            }
        }
        """
        return self._connection_nodes(query, connection_path=("teams",), limit=limit)

    def issues(
        self,
        team_key: str | None = None,
        assignee: str | None = None,
        state: str | None = None,
        limit: int = 50,
        include_archived: bool = False,
    ) -> list[dict[str, Any]]:
        """List issues with optional filters."""
        filters = []
        if team_key:
            filters.append(_team_key_filter(team_key))
        if assignee:
            if assignee.lower() == "me":
                filters.append("assignee: { isMe: { eq: true } }")
            else:
                filters.append(
                    f"assignee: {{ name: {{ containsIgnoreCase: {_linear_string_literal(assignee)} }} }}"
                )
        if state:
            filters.append(
                f"state: {{ name: {{ containsIgnoreCase: {_linear_string_literal(state)} }} }}"
            )

        filter_arg = f"filter: {{ {', '.join(filters)} }}, " if filters else ""
        query = f"""
        query Issues($first: Int!, $after: String, $includeArchived: Boolean) {{
            issues(
                {filter_arg}
                first: $first,
                after: $after,
                includeArchived: $includeArchived,
                orderBy: updatedAt
            ) {{
                nodes {{
                    id
                    identifier
                    title
                    description
                    priority
                    priorityLabel
                    state {{ id name color }}
                    assignee {{ id name }}
                    team {{ id name key }}
                    project {{ id name }}
                    cycle {{ id name number }}
                    labels {{ nodes {{ id name color }} }}
                    dueDate
                    createdAt
                    updatedAt
                    url
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._connection_nodes(
            query,
            connection_path=("issues",),
            variables={"includeArchived": include_archived},
            limit=limit,
        )

    def issue(self, issue_id: str) -> dict[str, Any]:
        """Get a single issue by ID or identifier."""
        query = """
        query Issue($id: String!) {
            issue(id: $id) {
                id
                identifier
                title
                description
                priority
                priorityLabel
                state { id name color }
                assignee { id name }
                team { id name key }
                project { id name }
                cycle { id name number }
                labels { nodes { id name color } }
                comments { nodes { id body user { name } createdAt } }
                parent { id identifier title }
                children { nodes { id identifier title state { name } } }
                dueDate
                createdAt
                updatedAt
                url
            }
        }
        """
        return self._query(query, {"id": issue_id}).get("issue", {})

    def fetch_asset(self, url: str, filename: str | None = None) -> dict[str, Any]:
        """Download a Linear-hosted asset such as an embedded screenshot."""
        if not url.startswith(UPLOADS_PREFIX):
            raise ValueError(
                f"fetch_asset only retrieves {UPLOADS_PREFIX}... URLs; got {url!r}"
            )

        resp = httpx.get(
            url,
            headers={"Authorization": self.api_key},
            follow_redirects=False,
            timeout=30.0,
        )
        location = resp.headers.get("location")
        if resp.is_redirect and location:
            resp = httpx.get(location, follow_redirects=True, timeout=30.0)
        resp.raise_for_status()

        content = resp.content
        mime_type = (
            resp.headers.get("content-type", "application/octet-stream")
            .split(";")[0]
            .strip()
        )
        if not filename:
            stem = urlparse(url).path.rstrip("/").rsplit("/", 1)[-1] or "asset"
            ext = mimetypes.guess_extension(mime_type) or ""
            filename = f"linear-{stem[:16]}{ext}"

        return {
            "data": base64.b64encode(content).decode(),
            "mime_type": mime_type,
            "filename": filename,
            "byte_length": len(content),
        }

    def projects(self, limit: int = 50) -> list[dict[str, Any]]:
        """List projects."""
        query = """
        query Projects($first: Int!, $after: String) {
            projects(first: $first, after: $after, orderBy: updatedAt) {
                nodes {
                    id
                    name
                    description
                    state
                    progress
                    startDate
                    targetDate
                    lead { id name }
                    teams { nodes { id name key } }
                    url
                }
                pageInfo { hasNextPage endCursor }
            }
        }
        """
        return self._connection_nodes(query, connection_path=("projects",), limit=limit)

    def list_etl_projects(
        self,
        *,
        page_size: int = 100,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]:
        """List one page of projects with the fields needed by the ETL."""
        filter_arg = ""
        variables: dict[str, Any] = {
            "first": page_size,
            "after": cursor,
            "includeArchived": include_archived,
        }
        updated_after_value = _linear_datetime(updated_after)
        updated_after_var = ""
        if updated_after_value:
            filter_arg = "filter: { updatedAt: { gte: $updatedAfter } },"
            updated_after_var = ",\n            $updatedAfter: DateTimeOrDuration"
            variables["updatedAfter"] = updated_after_value

        query = f"""
        query LinearEtlProjects(
            $first: Int!,
            $after: String,
            $includeArchived: Boolean
            {updated_after_var}
        ) {{
            projects(
                first: $first,
                after: $after,
                includeArchived: $includeArchived,
                orderBy: updatedAt,
                {filter_arg}
            ) {{
                nodes {{
                    id
                    name
                    description
                    slugId
                    state
                    status {{ id name type color }}
                    lead {{ id name displayName email }}
                    teams {{ nodes {{ id name key }} }}
                    createdAt
                    updatedAt
                    archivedAt
                    completedAt
                    canceledAt
                    url
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._query(query, variables).get("projects", {})

    def project(self, project_id: str) -> dict[str, Any]:
        """Get a single project."""
        query = """
        query Project($id: String!) {
            project(id: $id) {
                id
                name
                description
                state
                progress
                startDate
                targetDate
                lead { id name }
                teams { nodes { id name key } }
                issues { nodes { id identifier title state { name } } }
                url
            }
        }
        """
        return self._query(query, {"id": project_id}).get("project", {})

    def cycles(
        self, team_key: str | None = None, limit: int = 20
    ) -> list[dict[str, Any]]:
        """List cycles, optionally filtered by team."""
        filter_arg = ""
        if team_key:
            filter_arg = f"filter: {{ {_team_key_filter(team_key)} }}, "

        query = f"""
        query Cycles($first: Int!, $after: String) {{
            cycles({filter_arg}first: $first, after: $after, orderBy: updatedAt) {{
                nodes {{
                    id
                    name
                    number
                    startsAt
                    endsAt
                    progress
                    team {{ id name key }}
                    issues {{ nodes {{ id identifier title state {{ name }} }} }}
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._connection_nodes(query, connection_path=("cycles",), limit=limit)

    def workflow_states(self, team_key: str | None = None) -> list[dict[str, Any]]:
        """List workflow states, optionally filtered by team."""
        filter_arg = ""
        if team_key:
            filter_arg = f"filter: {{ {_team_key_filter(team_key)} }}, "

        query = f"""
        query WorkflowStates($first: Int!, $after: String) {{
            workflowStates({filter_arg}first: $first, after: $after) {{
                nodes {{
                    id
                    name
                    color
                    type
                    position
                    team {{ id name key }}
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._connection_nodes(
            query, connection_path=("workflowStates",), limit=100
        )

    def labels(self, team_key: str | None = None) -> list[dict[str, Any]]:
        """List issue labels, optionally filtered by team."""
        filter_arg = ""
        if team_key:
            filter_arg = f"filter: {{ {_team_key_filter(team_key)} }}, "

        query = f"""
        query Labels($first: Int!, $after: String) {{
            issueLabels({filter_arg}first: $first, after: $after) {{
                nodes {{
                    id
                    name
                    color
                    team {{ id name key }}
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._connection_nodes(
            query, connection_path=("issueLabels",), limit=100
        )

    def users(self, limit: int = 100) -> list[dict[str, Any]]:
        """List workspace users."""
        query = """
        query Users($first: Int!, $after: String) {
            users(first: $first, after: $after) {
                nodes { id name email displayName active }
                pageInfo { hasNextPage endCursor }
            }
        }
        """
        return self._connection_nodes(query, connection_path=("users",), limit=limit)

    def search_issues(self, query_str: str, limit: int = 25) -> list[dict[str, Any]]:
        """Search issues by text."""
        query = """
        query SearchIssues($term: String!, $first: Int!, $after: String) {
            searchIssues(term: $term, first: $first, after: $after) {
                nodes {
                    id
                    identifier
                    title
                    state { name }
                    assignee { name }
                    team { key }
                    dueDate
                    url
                }
                pageInfo { hasNextPage endCursor }
            }
        }
        """
        return self._connection_nodes(
            query,
            connection_path=("searchIssues",),
            variables={"term": query_str},
            limit=limit,
        )

    def list_etl_issues(
        self,
        *,
        page_size: int = 100,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]:
        """List one page of issues with the fields needed by the ETL."""
        filter_arg = ""
        variables: dict[str, Any] = {
            "first": page_size,
            "after": cursor,
            "includeArchived": include_archived,
        }
        updated_after_value = _linear_datetime(updated_after)
        updated_after_var = ""
        if updated_after_value:
            filter_arg = "filter: { updatedAt: { gte: $updatedAfter } },"
            updated_after_var = ",\n            $updatedAfter: DateTimeOrDuration"
            variables["updatedAfter"] = updated_after_value

        query = f"""
        query LinearEtlIssues(
            $first: Int!,
            $after: String,
            $includeArchived: Boolean
            {updated_after_var}
        ) {{
            issues(
                first: $first,
                after: $after,
                includeArchived: $includeArchived,
                orderBy: updatedAt,
                {filter_arg}
            ) {{
                nodes {{
                    id
                    identifier
                    number
                    title
                    description
                    url
                    priority
                    priorityLabel
                    estimate
                    dueDate
                    team {{ id name key }}
                    project {{ id name }}
                    cycle {{ id name number }}
                    state {{ id name type color }}
                    assignee {{ id name displayName email }}
                    creator {{ id name displayName email }}
                    parent {{ id identifier title }}
                    createdAt
                    updatedAt
                    archivedAt
                    startedAt
                    completedAt
                    canceledAt
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._query(query, variables).get("issues", {})

    def list_etl_comments(
        self,
        *,
        page_size: int = 100,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]:
        """List one page of comments with the fields needed by the ETL."""
        filter_arg = ""
        variables: dict[str, Any] = {
            "first": page_size,
            "after": cursor,
            "includeArchived": include_archived,
        }
        updated_after_value = _linear_datetime(updated_after)
        updated_after_var = ""
        if updated_after_value:
            filter_arg = "filter: { updatedAt: { gte: $updatedAfter } },"
            updated_after_var = ",\n            $updatedAfter: DateTimeOrDuration"
            variables["updatedAfter"] = updated_after_value

        query = f"""
        query LinearEtlComments(
            $first: Int!,
            $after: String,
            $includeArchived: Boolean
            {updated_after_var}
        ) {{
            comments(
                first: $first,
                after: $after,
                includeArchived: $includeArchived,
                orderBy: updatedAt,
                {filter_arg}
            ) {{
                nodes {{
                    id
                    body
                    url
                    issueId
                    projectId
                    parentId
                    user {{ id name displayName email }}
                    createdAt
                    updatedAt
                    archivedAt
                    editedAt
                    resolvedAt
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        }}
        """
        return self._query(query, variables).get("comments", {})
