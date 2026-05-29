"""Sentry HTTP API client for read-only issue perusal.

Wraps the parts of the Sentry REST API needed to browse issues without any of the
AI-powered search/Seer tooling: list/search issues with Sentry's native query syntax,
read issue details, list an issue's events, and pull a single event's full
stacktrace and breadcrumbs.
"""

import os
from typing import Any
from urllib.parse import quote

import httpx

from centaur_sdk import secret


class SentryClient:
    """Client for the Sentry REST API (SaaS by default).

    Authenticates with a Sentry user auth token via ``Authorization: Bearer``.
    Reads the token from ``SENTRY_AUTH_TOKEN``; the base URL defaults to
    ``https://sentry.io`` and can be overridden with ``SENTRY_URL`` for self-hosted.
    """

    def __init__(
        self,
        url: str | None = None,
        auth_token: str | None = None,
        timeout: float = 30.0,
    ):
        self._url = url
        self._auth_token = auth_token
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        url = (self._url or os.getenv("SENTRY_URL", "https://sentry.io")).rstrip("/")  # noqa: TID251
        if url and not url.startswith(("http://", "https://")):
            url = f"https://{url}"
        return f"{url}/api/0"

    def _auth_headers(self) -> dict[str, str]:
        token = self._auth_token or secret("SENTRY_AUTH_TOKEN", "")
        return {"Authorization": f"Bearer {token}"} if token else {}

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(
                base_url=self.base_url,
                headers=self._auth_headers(),
                timeout=self.timeout,
                follow_redirects=True,
            )
        return self._client

    def _request(
        self,
        method: str,
        path: str,
        params: dict | None = None,
    ) -> Any:
        clean = {k: v for k, v in (params or {}).items() if v is not None}
        resp = self.client.request(method, path, params=clean)
        if resp.status_code >= 400:
            raise RuntimeError(f"Sentry API error ({resp.status_code}): {resp.text}")
        return resp.json()

    # -- Navigation ----------------------------------------------------------

    def list_organizations(self) -> list[dict]:
        """List Sentry organizations the auth token can access."""
        return self._request("GET", "/organizations/")

    def list_projects(self, organization_slug: str) -> list[dict]:
        """List projects in an organization. Use to find project slugs for list_issues."""
        return self._request("GET", f"/organizations/{organization_slug}/projects/")

    # -- Issues --------------------------------------------------------------

    def list_issues(
        self,
        organization_slug: str,
        project_slug: str | None = None,
        query: str = "is:unresolved",
        sort: str = "date",
        stats_period: str = "14d",
        limit: int = 25,
    ) -> list[dict]:
        """List/search issues using Sentry's native search syntax (no AI translation).

        Args:
            organization_slug: Org slug (see list_organizations).
            project_slug: Restrict to one project (see list_projects). Omit to search all
                projects the token can access.
            query: Raw Sentry issue search, e.g. 'is:unresolved level:error environment:prod'.
            sort: 'date' (last seen), 'new', 'freq', 'user', or 'priority'.
            stats_period: Relative window, e.g. '24h', '14d', '90d'.
            limit: Max issues to return (per_page; cursor pagination not exposed).
        """
        params: dict[str, Any] = {
            "query": query,
            "sort": sort,
            "statsPeriod": stats_period,
            "limit": limit,
        }
        if project_slug:
            path = f"/projects/{organization_slug}/{project_slug}/issues/"
        else:
            path = f"/organizations/{organization_slug}/issues/"
            params["project"] = -1
        return self._request("GET", path, params=params)

    def get_issue(self, organization_slug: str, issue_id: str) -> dict:
        """Get full details for a single issue.

        Args:
            organization_slug: Org slug.
            issue_id: Numeric id or short id (e.g. 'PROJECT-1A').
        """
        return self._request(
            "GET", f"/organizations/{organization_slug}/issues/{issue_id}/"
        )

    def list_issue_events(
        self,
        organization_slug: str,
        issue_id: str,
        full: bool = False,
        limit: int = 25,
    ) -> list[dict]:
        """List events (individual occurrences) for an issue.

        Args:
            organization_slug: Org slug.
            issue_id: Numeric or short issue id.
            full: Include the full event payload for each occurrence.
            limit: Max events (per_page; cursor pagination not exposed).
        """
        return self._request(
            "GET",
            f"/organizations/{organization_slug}/issues/{issue_id}/events/",
            params={"full": "true" if full else "false", "per_page": limit},
        )

    def get_event(
        self,
        organization_slug: str,
        issue_id: str,
        event_id: str = "latest",
    ) -> dict:
        """Get one event for an issue with full stacktrace and breadcrumbs.

        Args:
            organization_slug: Org slug.
            issue_id: Numeric or short issue id.
            event_id: A specific event id, or 'latest'/'oldest'. Defaults to 'latest'.
        """
        return self._request(
            "GET",
            f"/organizations/{organization_slug}/issues/{issue_id}/events/{event_id}/",
        )

    def get_issue_tag_values(
        self,
        organization_slug: str,
        issue_id: str,
        tag_key: str,
    ) -> dict:
        """Get the value distribution for a tag on an issue (e.g. 'release', 'browser')."""
        return self._request(
            "GET",
            f"/organizations/{organization_slug}/issues/{issue_id}/tags/{quote(tag_key)}/",
        )

    # -- Lifecycle -----------------------------------------------------------

    def close(self):
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> SentryClient:
    return SentryClient()
