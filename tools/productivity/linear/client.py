"""Linear GraphQL API client for the Linear tool."""

from __future__ import annotations

from typing import Any

from workflows.linear.readonly import LinearReadonlyClient


class LinearClient(LinearReadonlyClient):
    """Tool-facing Linear client.

    Read-only GraphQL methods live in ``workflows.linear.readonly`` so workflows
    can reuse them. Tool-only mutations stay here.
    """

    def me(self) -> dict[str, Any]:
        """Get authenticated user info."""
        return super().me()

    def teams(self, limit: int = 50) -> list[dict[str, Any]]:
        """List teams."""
        return super().teams(limit=limit)

    def issues(
        self,
        team_key: str | None = None,
        assignee: str | None = None,
        state: str | None = None,
        limit: int = 50,
        include_archived: bool = False,
    ) -> list[dict[str, Any]]:
        """List issues with optional filters."""
        return super().issues(
            team_key=team_key,
            assignee=assignee,
            state=state,
            limit=limit,
            include_archived=include_archived,
        )

    def issue(self, issue_id: str) -> dict[str, Any]:
        """Get a single issue by ID or identifier."""
        return super().issue(issue_id)

    def fetch_asset(self, url: str, filename: str | None = None) -> dict[str, Any]:
        """Download a Linear-hosted asset such as an embedded screenshot."""
        return super().fetch_asset(url, filename=filename)

    def projects(self, limit: int = 50) -> list[dict[str, Any]]:
        """List projects."""
        return super().projects(limit=limit)

    def project(self, project_id: str) -> dict[str, Any]:
        """Get a single project."""
        return super().project(project_id)

    def cycles(self, team_key: str | None = None, limit: int = 20) -> list[dict[str, Any]]:
        """List cycles, optionally filtered by team."""
        return super().cycles(team_key=team_key, limit=limit)

    def workflow_states(self, team_key: str | None = None) -> list[dict[str, Any]]:
        """List workflow states, optionally filtered by team."""
        return super().workflow_states(team_key=team_key)

    def labels(self, team_key: str | None = None) -> list[dict[str, Any]]:
        """List issue labels, optionally filtered by team."""
        return super().labels(team_key=team_key)

    def users(self, limit: int = 100) -> list[dict[str, Any]]:
        """List workspace users."""
        return super().users(limit=limit)

    def search_issues(self, query_str: str, limit: int = 25) -> list[dict[str, Any]]:
        """Search issues by text."""
        return super().search_issues(query_str=query_str, limit=limit)

    @staticmethod
    def _mutation_result(result: dict[str, Any], key: str, entity: str = "issue") -> dict[str, Any]:
        """Flatten a {success, <entity>} mutation payload into the entity
        fields plus a top-level ``success`` flag, so callers can both read the
        fields directly and detect mutations that fail without a GraphQL error.
        """
        payload = result.get(key, {})
        return {"success": payload.get("success", False), **(payload.get(entity) or {})}

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
        return self._mutation_result(result, "issueCreate")

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
        return self._mutation_result(result, "issueUpdate")

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
        return self._mutation_result(result, "commentCreate", "comment")

    def _resolve_label_ids(self, names: list[str], team_key: str | None = None) -> dict[str, str]:
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
        nodes = self._query(query, {"names": names}).get("issueLabels", {}).get("nodes", [])

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
                f"ambiguous label(s): {', '.join(dup)}. Each must exist exactly once in its scope."
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
