"""Linear GraphQL API client."""

import base64
import mimetypes
from typing import Any
from urllib.parse import urlparse

import httpx

from centaur_sdk import secret

UPLOADS_PREFIX = "https://uploads.linear.app/"

GRAPHQL_ENDPOINT = "https://api.linear.app/graphql"


class LinearClient:
    """Client for Linear's GraphQL API."""

    def __init__(self, api_key: str | None = None):
        self.api_key = api_key or secret("LINEAR_API_KEY", "")
        if not self.api_key:
            raise RuntimeError(
                "LINEAR_API_KEY not set.\n"
                "Get one at https://linear.app/settings/account/security → Personal API Keys"
            )
        self._http = httpx.Client(
            base_url=GRAPHQL_ENDPOINT,
            headers={"Authorization": self.api_key, "Content-Type": "application/json"},
            timeout=10.0,
        )

    def _query(self, query: str, variables: dict[str, Any] | None = None) -> dict[str, Any]:
        """Execute a GraphQL query."""
        resp = self._http.post("", json={"query": query, "variables": variables or {}})
        resp.raise_for_status()
        data = resp.json()
        if "errors" in data:
            errors = data["errors"]
            msg = errors[0].get("message", str(errors))
            raise RuntimeError(f"Linear API error: {msg}")
        return data.get("data", {})

    def me(self) -> dict[str, Any]:
        """Get authenticated user info."""
        query = """
        query Me {
            viewer { id name email }
        }
        """
        return self._query(query).get("viewer", {})

    def teams(self, limit: int = 50) -> list[dict[str, Any]]:
        """List all teams."""
        query = """
        query Teams($first: Int!) {
            teams(first: $first) {
                nodes { id name key description }
            }
        }
        """
        return self._query(query, {"first": limit}).get("teams", {}).get("nodes", [])

    def issues(
        self,
        team_key: str | None = None,
        assignee: str | None = None,
        state: str | None = None,
        limit: int = 50,
        include_archived: bool = False,
    ) -> list[dict[str, Any]]:
        """List issues with optional filters.

        Args:
            team_key: Filter by team key (e.g., "ENG")
            assignee: Filter by assignee name or "me"
            state: Filter by state name (e.g., "In Progress", "Done")
            limit: Max results
            include_archived: Include archived issues
        """
        filters = []
        if team_key:
            filters.append(f'team: {{ key: {{ eq: "{team_key}" }} }}')
        if assignee:
            if assignee.lower() == "me":
                filters.append("assignee: { isMe: { eq: true } }")
            else:
                filters.append(f'assignee: {{ name: {{ containsIgnoreCase: "{assignee}" }} }}')
        if state:
            filters.append(f'state: {{ name: {{ containsIgnoreCase: "{state}" }} }}')

        filter_str = ", ".join(filters)
        filter_arg = f"filter: {{ {filter_str} }}, " if filters else ""

        query = f"""
        query Issues($first: Int!, $includeArchived: Boolean) {{
            issues({filter_arg}first: $first, includeArchived: $includeArchived, orderBy: updatedAt) {{
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
            }}
        }}
        """
        return (
            self._query(query, {"first": limit, "includeArchived": include_archived})
            .get("issues", {})
            .get("nodes", [])
        )

    def issue(self, issue_id: str) -> dict[str, Any]:
        """Get a single issue by ID or identifier (e.g., ENG-123)."""
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
        """Download a Linear-hosted asset (e.g. a screenshot embedded in an
        issue description or comment) so it can be viewed.

        Inline images in Linear render as bare ``https://uploads.linear.app/...``
        URLs that require the same ``Authorization`` header as the GraphQL API,
        so an unauthenticated ``curl`` from a sandbox gets a 401. This fetches
        the bytes with the API key and returns them as an attachment: assets
        larger than ~64 KB come back as a ``download_url`` you pull locally
        (``curl http://api:8000<download_url> -o shot.png``) and open with
        ``look_at``; smaller ones are inlined as base64 in ``data``.

        Args:
            url: A ``https://uploads.linear.app/...`` asset URL.
            filename: Optional name for the saved attachment; derived from the
                URL and content type when omitted.
        """
        if not url.startswith(UPLOADS_PREFIX):
            raise ValueError(
                f"fetch_asset only retrieves {UPLOADS_PREFIX}... URLs; got {url!r}"
            )

        # uploads.linear.app gates on the API key, then may 302 to a
        # self-authorizing CDN URL. Follow that hop WITHOUT the auth header so
        # it doesn't collide with the signed-URL credentials — this mirrors
        # curl -L, which drops the Authorization header on cross-host redirects.
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

    def create_issue(
        self,
        title: str,
        team_id: str,
        description: str | None = None,
        assignee_id: str | None = None,
        state_id: str | None = None,
        priority: int | None = None,
        label_ids: list[str] | None = None,
        project_id: str | None = None,
        cycle_id: str | None = None,
        parent_id: str | None = None,
        due_date: str | None = None,
    ) -> dict[str, Any]:
        """Create a new issue.

        Args:
            due_date: Due date as YYYY-MM-DD.
        """
        mutation = """
        mutation IssueCreate($input: IssueCreateInput!) {
            issueCreate(input: $input) {
                success
                issue { id identifier title dueDate url }
            }
        }
        """
        input_data: dict[str, Any] = {"title": title, "teamId": team_id}
        if description:
            input_data["description"] = description
        if assignee_id:
            input_data["assigneeId"] = assignee_id
        if state_id:
            input_data["stateId"] = state_id
        if priority is not None:
            input_data["priority"] = priority
        if label_ids:
            input_data["labelIds"] = label_ids
        if project_id:
            input_data["projectId"] = project_id
        if cycle_id:
            input_data["cycleId"] = cycle_id
        if parent_id:
            input_data["parentId"] = parent_id
        if due_date:
            input_data["dueDate"] = due_date

        result = self._query(mutation, {"input": input_data})
        return result.get("issueCreate", {}).get("issue", {})

    def update_issue(
        self,
        issue_id: str,
        title: str | None = None,
        description: str | None = None,
        state_id: str | None = None,
        assignee_id: str | None = None,
        priority: int | None = None,
        project_id: str | None = None,
        due_date: str | None = None,
    ) -> dict[str, Any]:
        """Update an existing issue.

        Args:
            due_date: Due date as YYYY-MM-DD.
        """
        mutation = """
        mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
            issueUpdate(id: $id, input: $input) {
                success
                issue { id identifier title dueDate state { name } project { id name } url }
            }
        }
        """
        input_data: dict[str, Any] = {}
        if title:
            input_data["title"] = title
        if description:
            input_data["description"] = description
        if state_id:
            input_data["stateId"] = state_id
        if assignee_id:
            input_data["assigneeId"] = assignee_id
        if priority is not None:
            input_data["priority"] = priority
        if project_id:
            input_data["projectId"] = project_id
        if due_date:
            input_data["dueDate"] = due_date

        result = self._query(mutation, {"id": issue_id, "input": input_data})
        return result.get("issueUpdate", {}).get("issue", {})

    def add_comment(self, issue_id: str, body: str) -> dict[str, Any]:
        """Add a comment to an issue."""
        mutation = """
        mutation CommentCreate($input: CommentCreateInput!) {
            commentCreate(input: $input) {
                success
                comment { id body createdAt }
            }
        }
        """
        result = self._query(mutation, {"input": {"issueId": issue_id, "body": body}})
        return result.get("commentCreate", {}).get("comment", {})

    def projects(self, limit: int = 50) -> list[dict[str, Any]]:
        """List all projects."""
        query = """
        query Projects($first: Int!) {
            projects(first: $first, orderBy: updatedAt) {
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
            }
        }
        """
        return self._query(query, {"first": limit}).get("projects", {}).get("nodes", [])

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

    def cycles(self, team_key: str | None = None, limit: int = 20) -> list[dict[str, Any]]:
        """List cycles, optionally filtered by team."""
        filter_str = ""
        if team_key:
            filter_str = f'filter: {{ team: {{ key: {{ eq: "{team_key}" }} }} }}, '

        query = f"""
        query Cycles($first: Int!) {{
            cycles({filter_str}first: $first, orderBy: updatedAt) {{
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
            }}
        }}
        """
        return self._query(query, {"first": limit}).get("cycles", {}).get("nodes", [])

    def workflow_states(self, team_key: str | None = None) -> list[dict[str, Any]]:
        """List workflow states, optionally filtered by team."""
        filter_str = ""
        if team_key:
            filter_str = f'filter: {{ team: {{ key: {{ eq: "{team_key}" }} }} }}, '

        query = f"""
        query WorkflowStates {{
            workflowStates({filter_str}first: 100) {{
                nodes {{
                    id
                    name
                    color
                    type
                    position
                    team {{ id name key }}
                }}
            }}
        }}
        """
        return self._query(query).get("workflowStates", {}).get("nodes", [])

    def labels(self, team_key: str | None = None) -> list[dict[str, Any]]:
        """List labels, optionally filtered by team."""
        filter_str = ""
        if team_key:
            filter_str = f'filter: {{ team: {{ key: {{ eq: "{team_key}" }} }} }}, '

        query = f"""
        query Labels {{
            issueLabels({filter_str}first: 100) {{
                nodes {{
                    id
                    name
                    color
                    team {{ id name key }}
                }}
            }}
        }}
        """
        return self._query(query).get("issueLabels", {}).get("nodes", [])

    def _resolve_label_ids(
        self, names: list[str], team_key: str | None = None
    ) -> dict[str, str]:
        """Resolve label names to IDs, preferring a team-scoped label over a
        workspace label of the same name. Raises if any requested name is
        missing, or is ambiguous within its chosen scope.
        """
        if not names:
            return {}
        query = """
        query Labels($names: [String!]) {
            issueLabels(filter: { name: { in: $names } }, first: 250) {
                nodes { id name team { key } }
            }
        }
        """
        nodes = (
            self._query(query, {"names": names}).get("issueLabels", {}).get("nodes", [])
        )

        team_hits: dict[str, list[str]] = {n: [] for n in names}
        workspace_hits: dict[str, list[str]] = {n: [] for n in names}
        for node in nodes:
            name = node.get("name")
            if name not in team_hits:
                continue
            node_team = node.get("team")
            if not node_team:
                workspace_hits[name].append(node["id"])
            elif team_key and node_team.get("key") == team_key:
                team_hits[name].append(node["id"])

        resolved: dict[str, str] = {}
        missing: list[str] = []
        dup: list[str] = []
        for name in names:
            source = team_hits[name] if team_hits[name] else workspace_hits[name]
            if not source:
                missing.append(name)
            elif len(source) > 1:
                scope = f"team {team_key}" if team_hits[name] else "workspace"
                dup.append(f"{name} ({scope})")
            else:
                resolved[name] = source[0]

        if missing:
            raise RuntimeError(
                f"missing label(s): {', '.join(missing)}. "
                f"Create them in team {team_key or '<workspace>'} or at the workspace level."
            )
        if dup:
            raise RuntimeError(
                f"ambiguous label(s): {', '.join(dup)}. "
                "Each must exist exactly once in its scope."
            )
        return resolved

    def add_label(
        self, issue_id: str, label_name: str, team_key: str | None = None
    ) -> dict[str, Any]:
        """Add a single label (by name) to an issue, leaving its other labels
        untouched. Prefer this over ``update_issue(label_ids=...)`` for
        incremental changes, since ``issueUpdate`` replaces the full label set.

        Pass ``team_key`` to bind to a team-scoped label when a workspace label
        of the same name also exists.
        """
        label_id = self._resolve_label_ids([label_name], team_key)[label_name]
        mutation = """
        mutation AddLabel($id: String!, $labelId: String!) {
            issueAddLabel(id: $id, labelId: $labelId) { success }
        }
        """
        result = self._query(mutation, {"id": issue_id, "labelId": label_id})
        return {"success": result.get("issueAddLabel", {}).get("success", False)}

    def remove_label(
        self, issue_id: str, label_name: str, team_key: str | None = None
    ) -> dict[str, Any]:
        """Remove a single label (by name) from an issue, leaving its other
        labels untouched. Succeeds even if the label isn't currently applied;
        raises only if no label by that name exists in the chosen scope.
        """
        label_id = self._resolve_label_ids([label_name], team_key)[label_name]
        mutation = """
        mutation RemoveLabel($id: String!, $labelId: String!) {
            issueRemoveLabel(id: $id, labelId: $labelId) { success }
        }
        """
        result = self._query(mutation, {"id": issue_id, "labelId": label_id})
        return {"success": result.get("issueRemoveLabel", {}).get("success", False)}

    def users(self, limit: int = 100) -> list[dict[str, Any]]:
        """List workspace users."""
        query = """
        query Users($first: Int!) {
            users(first: $first) {
                nodes { id name email displayName active }
            }
        }
        """
        return self._query(query, {"first": limit}).get("users", {}).get("nodes", [])

    def search_issues(self, query_str: str, limit: int = 25) -> list[dict[str, Any]]:
        """Search issues by text."""
        query = """
        query SearchIssues($query: String!, $first: Int!) {
            searchIssues(query: $query, first: $first) {
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
            }
        }
        """
        return (
            self._query(query, {"query": query_str, "first": limit})
            .get("searchIssues", {})
            .get("nodes", [])
        )

    def create_issue_relation(
        self,
        issue_id: str,
        related_issue_id: str,
        relation_type: str,
    ) -> dict[str, Any]:
        """Create a relation between two issues.

        Args:
            issue_id: The issue identifier (e.g., "ENG-123")
            related_issue_id: The related issue identifier (e.g., "ENG-456")
            relation_type: Type of relation: "blocks", "duplicate", "related"

        For "blocks" type:
            - issue_id blocks related_issue_id
            - (i.e., related_issue_id is blocked by issue_id)
        """
        mutation = """
        mutation IssueRelationCreate($input: IssueRelationCreateInput!) {
            issueRelationCreate(input: $input) {
                success
                issueRelation {
                    id
                    type
                    issue { id identifier title }
                    relatedIssue { id identifier title }
                }
            }
        }
        """
        input_data = {
            "issueId": issue_id,
            "relatedIssueId": related_issue_id,
            "type": relation_type,
        }
        result = self._query(mutation, {"input": input_data})
        return result.get("issueRelationCreate", {})



def _client() -> LinearClient:
    return LinearClient()
