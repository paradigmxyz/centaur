"""Railway GraphQL API client for read-only deployment inspection."""

from __future__ import annotations

from typing import Any, Literal

import httpx

from centaur_sdk import secret

API_BASE = "https://backboard.railway.com/graphql/v2"
TokenKind = Literal["account", "project"]


class RailwayClient:
    """Read-only client for Railway's GraphQL API.

    Railway supports account/workspace tokens through the ``Authorization``
    header and project-scoped tokens through ``Project-Access-Token``. This
    client can use either mode, but exposes only query operations.
    """

    def __init__(
        self,
        api_token: str | None = None,
        project_token: str | None = None,
        base_url: str = API_BASE,
        timeout: float = 30.0,
    ):
        self._api_token = api_token
        self._project_token = project_token
        self.base_url = base_url
        self.timeout = timeout
        self._client: httpx.Client | None = None

    def _token(self, token_kind: TokenKind) -> str:
        if token_kind == "project":
            token = self._project_token or secret("RAILWAY_PROJECT_TOKEN", "")
            if not token:
                token = self._api_token or secret("RAILWAY_TOKEN", "")
        else:
            token = self._api_token or secret("RAILWAY_TOKEN", "")
        token = token.strip()
        if not token:
            raise RuntimeError("RAILWAY_TOKEN not set.")
        return token

    def _headers(self, token_kind: TokenKind) -> dict[str, str]:
        headers = {"Content-Type": "application/json"}
        token = self._token(token_kind)
        if token_kind == "project":
            headers["Project-Access-Token"] = token
        else:
            headers["Authorization"] = f"Bearer {token}"
        return headers

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout, follow_redirects=True)
        return self._client

    @staticmethod
    def _assert_read_only(query: str) -> None:
        stripped = query.lstrip()
        lowered = stripped.lower()
        if not stripped:
            raise ValueError("GraphQL query is required.")
        if lowered.startswith(("mutation", "subscription")):
            raise ValueError("Railway tool only allows read-only GraphQL queries.")
        if not (lowered.startswith("query") or stripped.startswith("{")):
            raise ValueError("Railway GraphQL operations must be queries.")

    def graphql(
        self,
        query: str,
        variables: dict[str, Any] | None = None,
        token_kind: TokenKind = "account",
    ) -> dict[str, Any]:
        """Run a read-only Railway GraphQL query.

        Args:
            query: A GraphQL query operation. Mutations and subscriptions are rejected.
            variables: Optional GraphQL variables.
            token_kind: ``account`` for bearer tokens or ``project`` for
                project-scoped tokens.
        """
        self._assert_read_only(query)
        response = self.client.post(
            self.base_url,
            headers=self._headers(token_kind),
            json={"query": query, "variables": variables or {}},
        )
        if response.status_code >= 400:
            raise RuntimeError(f"Railway API error ({response.status_code}): {response.text}")
        return response.json()

    def whoami(self) -> dict[str, Any]:
        """Return the account identity visible to an account/workspace token."""
        return self.graphql(
            """
            query RailwayViewer {
              me {
                id
                name
                email
              }
            }
            """
        )

    def project_token_info(self) -> dict[str, Any]:
        """Return project/environment metadata visible to a project token."""
        return self.graphql(
            """
            query RailwayProjectTokenInfo {
              projectToken {
                projectId
                environmentId
                project {
                  id
                  name
                }
                environment {
                  id
                  name
                }
              }
            }
            """,
            token_kind="project",
        )

    def list_projects(self, first: int = 25) -> dict[str, Any]:
        """List projects visible to the token."""
        return self.graphql(
            """
            query RailwayProjects($first: Int!) {
              projects(first: $first) {
                edges {
                  node {
                    id
                    name
                    createdAt
                    updatedAt
                  }
                }
                pageInfo {
                  hasNextPage
                  endCursor
                }
              }
            }
            """,
            {"first": first},
        )

    def get_project(self, project_id: str) -> dict[str, Any]:
        """Get a project's services and environments."""
        return self.graphql(
            """
            query RailwayProject($projectId: String!) {
              project(id: $projectId) {
                id
                name
                createdAt
                updatedAt
                services {
                  edges {
                    node {
                      id
                      name
                      createdAt
                      updatedAt
                    }
                  }
                }
                environments {
                  edges {
                    node {
                      id
                      name
                    }
                  }
                }
              }
            }
            """,
            {"projectId": project_id},
        )

    def list_deployments(
        self,
        project_id: str,
        service_id: str | None = None,
        environment_id: str | None = None,
        first: int = 20,
    ) -> dict[str, Any]:
        """List recent deployments for a Railway project, service, or environment."""
        input_payload: dict[str, Any] = {"projectId": project_id}
        if service_id:
            input_payload["serviceId"] = service_id
        if environment_id:
            input_payload["environmentId"] = environment_id
        return self.graphql(
            """
            query RailwayDeployments($input: DeploymentListInput!, $first: Int!) {
              deployments(input: $input, first: $first) {
                edges {
                  node {
                    id
                    status
                    createdAt
                    updatedAt
                    service {
                      id
                      name
                    }
                    environment {
                      id
                      name
                    }
                  }
                }
                pageInfo {
                  hasNextPage
                  endCursor
                }
              }
            }
            """,
            {"input": input_payload, "first": first},
        )

    def deployment_logs(
        self,
        deployment_id: str,
        limit: int = 100,
        filter_query: str | None = None,
    ) -> dict[str, Any]:
        """Fetch recent logs for a deployment."""
        return self.graphql(
            """
            query RailwayDeploymentLogs(
              $deploymentId: String!,
              $limit: Int!,
              $filter: String
            ) {
              deploymentLogs(
                deploymentId: $deploymentId,
                limit: $limit,
                filter: $filter
              ) {
                timestamp
                message
                severity
              }
            }
            """,
            {"deploymentId": deployment_id, "limit": limit, "filter": filter_query},
        )

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> RailwayClient:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


def _client() -> RailwayClient:
    return RailwayClient()
