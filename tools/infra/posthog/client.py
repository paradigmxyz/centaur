"""PostHog API client."""

import os

import httpx

from centaur_sdk import secret


def _is_placeholder_secret(value: str | None, name: str) -> bool:
    if not value:
        return True
    stripped = value.strip()
    return stripped == name or stripped.startswith(("__CENTAUR_", "CENTAUR_SECRET_"))


class PostHogClient:
    """Client for PostHog API.

    Uses HogQL queries for flexible analytics. Requires a personal API key
    with Query Read permissions.
    """

    def __init__(
        self,
        api_key: str | None = None,
        project_id: str | None = None,
        host: str | None = None,
        timeout: float = 60.0,
    ):
        """Initialize the PostHog client.

        Args:
            api_key: Personal API key (or set POSTHOG_API_KEY)
            project_id: Project ID (or set POSTHOG_PROJECT_ID)
            host: API host (default: us.posthog.com)
            timeout: Request timeout in seconds
        """
        self._api_key = api_key
        self._project_id = project_id
        self._host = host
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    @property
    def api_key(self) -> str:
        """Get API key from instance or env var."""
        if self._api_key:
            return self._api_key
        key = secret("POSTHOG_API_KEY", "")
        if key:
            return key
        raise RuntimeError("POSTHOG_API_KEY not set.")

    @property
    def project_id(self) -> str:
        """Get project ID from instance or env var."""
        if self._project_id and not _is_placeholder_secret(self._project_id, "POSTHOG_PROJECT_ID"):
            return self._project_id
        pid = secret("POSTHOG_PROJECT_ID", "")
        if not _is_placeholder_secret(pid, "POSTHOG_PROJECT_ID"):
            self._project_id = pid
            return pid
        self._project_id = self._discover_project_id()
        return self._project_id

    def _discover_project_id(self) -> str:
        """Use the first project visible to the API key when no project id is configured."""
        payload = self._request("GET", "/api/projects/")
        results = payload.get("results") if isinstance(payload, dict) else None
        if not results:
            raise RuntimeError("POSTHOG_PROJECT_ID not set and no PostHog projects were visible.")
        project_id = results[0].get("id")
        if project_id is None:
            raise RuntimeError("POSTHOG_PROJECT_ID not set and project discovery returned no id.")
        return str(project_id)

    @property
    def host(self) -> str:
        """Get API host."""
        if self._host:
            return self._host
        return os.getenv("POSTHOG_HOST", "us.posthog.com")  # noqa: TID251

    @property
    def base_url(self) -> str:
        return f"https://{self.host}"

    def _request(
        self,
        method: str,
        endpoint: str,
        json_data: dict | None = None,
        params: dict | None = None,
    ) -> dict | list:
        """Make an authenticated API request.

        Args:
            method: HTTP method (GET, POST, etc.)
            endpoint: API endpoint path
            json_data: JSON body for POST requests
            params: Query parameters

        Returns:
            JSON response data

        Raises:
            RuntimeError: If the request fails
        """
        url = f"{self.base_url}{endpoint}"
        headers = {"Authorization": f"Bearer {self.api_key}"}

        try:
            response = self.client.request(
                method, url, headers=headers, json=json_data, params=params
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"API error: {e.response.status_code} - {e.response.text}") from e
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}") from e

    def query(self, sql: str, name: str | None = None) -> dict:
        """Execute a HogQL query.

        Args:
            sql: HogQL SQL query
            name: Optional query name for logging

        Returns:
            Query results with columns and results
        """
        payload = {
            "query": {
                "kind": "HogQLQuery",
                "query": sql,
            }
        }
        if name:
            payload["name"] = name

        return self._request("POST", f"/api/projects/{self.project_id}/query/", json_data=payload)

    def events(
        self,
        event: str | None = None,
        properties: dict | None = None,
        limit: int = 100,
        after: str | None = None,
        before: str | None = None,
    ) -> list[dict]:
        """Query events using HogQL.

        Args:
            event: Filter by event name
            properties: Filter by properties
            limit: Max results
            after: Events after this datetime
            before: Events before this datetime

        Returns:
            List of events
        """
        conditions = []
        if event:
            conditions.append(f"event = '{event}'")
        if after:
            conditions.append(f"timestamp >= '{after}'")
        if before:
            conditions.append(f"timestamp <= '{before}'")

        where_clause = " AND ".join(conditions) if conditions else "1=1"
        sql = f"""
            SELECT timestamp, event, distinct_id, properties
            FROM events
            WHERE {where_clause}
            ORDER BY timestamp DESC
            LIMIT {limit}
        """
        return self.query(sql, name="events_query")

    def breakdown(
        self,
        event: str | None = None,
        property: str = "$browser",
        days: int = 7,
        limit: int = 20,
    ) -> dict:
        """Get event breakdown by a property.

        Args:
            event: Event name to filter (None for all events)
            property: Property to breakdown by (e.g., '$browser', '$os')
            days: Number of days to look back
            limit: Max results

        Returns:
            Query results with breakdown
        """
        event_filter = f"AND event = '{event}'" if event else ""
        sql = f"""
            SELECT
                properties.{property} AS value,
                count() AS count,
                round(count() * 100.0 / sum(count()) OVER (), 2) AS percentage
            FROM events
            WHERE timestamp >= now() - INTERVAL {days} DAY
            {event_filter}
            GROUP BY value
            ORDER BY count DESC
            LIMIT {limit}
        """
        return self.query(sql, name=f"breakdown_{property}")

    def pageviews(
        self,
        url_pattern: str | None = None,
        days: int = 7,
        limit: int = 20,
    ) -> dict:
        """Get pageview analytics.

        Args:
            url_pattern: Filter URLs containing this pattern
            days: Number of days to look back
            limit: Max results

        Returns:
            Pageview data by URL
        """
        url_filter = f"AND properties.$current_url LIKE '%{url_pattern}%'" if url_pattern else ""
        sql = f"""
            SELECT
                properties.$current_url AS url,
                count() AS views,
                uniq(distinct_id) AS unique_visitors
            FROM events
            WHERE event = '$pageview'
            AND timestamp >= now() - INTERVAL {days} DAY
            {url_filter}
            GROUP BY url
            ORDER BY views DESC
            LIMIT {limit}
        """
        return self.query(sql, name="pageviews")

    def user_agents(
        self,
        url_pattern: str | None = None,
        event: str = "$pageview",
        days: int = 7,
        limit: int = 20,
    ) -> dict:
        """Get user-agent breakdown.

        Args:
            url_pattern: Filter URLs containing this pattern
            event: Event type to filter
            days: Number of days to look back
            limit: Max results

        Returns:
            User-agent breakdown with counts and percentages
        """
        url_filter = f"AND properties.$current_url LIKE '%{url_pattern}%'" if url_pattern else ""
        sql = f"""
            SELECT
                properties.$browser AS browser,
                properties.$os AS os,
                count() AS count,
                round(count() * 100.0 / sum(count()) OVER (), 2) AS percentage
            FROM events
            WHERE event = '{event}'
            AND timestamp >= now() - INTERVAL {days} DAY
            {url_filter}
            GROUP BY browser, os
            ORDER BY count DESC
            LIMIT {limit}
        """
        return self.query(sql, name="user_agents")

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> PostHogClient:
    api_key = secret("POSTHOG_API_KEY", "")
    project_id = secret("POSTHOG_PROJECT_ID", "")
    return PostHogClient(api_key=api_key, project_id=project_id)
