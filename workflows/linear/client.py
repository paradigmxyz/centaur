from __future__ import annotations

from collections.abc import Sequence
from typing import Any

import httpx

from centaur_sdk import secret

UPLOADS_PREFIX = "https://uploads.linear.app/"
GRAPHQL_ENDPOINT = "https://api.linear.app/graphql"
DEFAULT_PAGE_SIZE = 100
MAX_PAGE_SIZE = 250


class LinearGraphQLClient:
    """Authenticated Linear GraphQL client shared by tools and ETL workflows."""

    def __init__(
        self,
        api_key: str | None = None,
        *,
        http_client: httpx.Client | None = None,
        timeout: float = 10.0,
    ) -> None:
        self.api_key = api_key or secret("LINEAR_API_KEY", "")
        if not self.api_key:
            raise RuntimeError(
                "LINEAR_API_KEY not set.\n"
                "Get one at https://linear.app/settings/account/security -> Personal API Keys"
            )
        self._http = http_client or httpx.Client(
            base_url=GRAPHQL_ENDPOINT,
            headers={"Authorization": self.api_key, "Content-Type": "application/json"},
            timeout=timeout,
        )

    def _query(
        self, query: str, variables: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Execute a Linear GraphQL query or mutation."""
        resp = self._http.post("", json={"query": query, "variables": variables or {}})
        resp.raise_for_status()
        data = resp.json()
        if "errors" in data:
            errors = data["errors"]
            msg = errors[0].get("message", str(errors))
            raise RuntimeError(f"Linear API error: {msg}")
        return data.get("data", {})

    def _connection_nodes(
        self,
        query: str,
        *,
        connection_path: Sequence[str],
        variables: dict[str, Any] | None = None,
        limit: int | None = None,
        page_size: int = DEFAULT_PAGE_SIZE,
    ) -> list[dict[str, Any]]:
        """Collect nodes from a cursor-paginated Linear connection."""
        if limit is not None and limit <= 0:
            return []

        requested_page_size = max(1, min(int(page_size), MAX_PAGE_SIZE))
        remaining = limit
        cursor: str | None = None
        nodes: list[dict[str, Any]] = []

        while remaining is None or remaining > 0:
            batch_size = requested_page_size
            if remaining is not None:
                batch_size = min(batch_size, remaining)

            page_variables = dict(variables or {})
            page_variables.update({"first": batch_size, "after": cursor})
            data = self._query(query, page_variables)

            connection: Any = data
            for key in connection_path:
                if not isinstance(connection, dict):
                    connection = {}
                    break
                connection = connection.get(key, {})
            if not isinstance(connection, dict):
                return nodes

            page_nodes = connection.get("nodes") or []
            nodes.extend(page_nodes)
            if remaining is not None:
                remaining -= len(page_nodes)

            page_info = connection.get("pageInfo") or {}
            cursor = page_info.get("endCursor")
            if not page_info.get("hasNextPage") or not cursor or not page_nodes:
                break

        return nodes
