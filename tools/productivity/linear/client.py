"""Linear GraphQL API client."""

from typing import Any

import httpx

from centaur_sdk import secret

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
