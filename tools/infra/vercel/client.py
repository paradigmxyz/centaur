"""Vercel REST API client for read-only deployment inspection."""

from __future__ import annotations

from typing import Any
from urllib.parse import quote

import httpx

from centaur_sdk import secret

API_BASE = "https://api.vercel.com"


class VercelClient:
    """Read-only client for Vercel projects and deployments."""

    def __init__(
        self,
        api_token: str | None = None,
        base_url: str = API_BASE,
        timeout: float = 30.0,
    ):
        self._api_token = api_token
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self._client: httpx.Client | None = None

    def _token(self) -> str:
        token = (self._api_token or secret("VERCEL_TOKEN", "")).strip()
        if not token:
            raise RuntimeError("VERCEL_TOKEN not set.")
        return token

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout, follow_redirects=True)
        return self._client

    @staticmethod
    def _team_params(team_id: str | None = None, slug: str | None = None) -> dict[str, str]:
        params: dict[str, str] = {}
        if team_id:
            params["teamId"] = team_id
        if slug:
            params["slug"] = slug
        return params

    def _request(self, path: str, params: dict[str, Any] | None = None) -> Any:
        clean_params = {k: v for k, v in (params or {}).items() if v is not None}
        response = self.client.get(
            f"{self.base_url}/{path.lstrip('/')}",
            headers={"Authorization": f"Bearer {self._token()}"},
            params=clean_params,
        )
        if response.status_code >= 400:
            raise RuntimeError(f"Vercel API error ({response.status_code}): {response.text}")
        return response.json()

    def get_user(self) -> dict[str, Any]:
        """Return the authenticated Vercel user."""
        return self._request("/v2/user")

    def list_teams(
        self,
        limit: int = 20,
        since: int | None = None,
        until: int | None = None,
    ) -> dict[str, Any]:
        """List teams visible to the token."""
        return self._request(
            "/v2/teams",
            {"limit": limit, "since": since, "until": until},
        )

    def list_projects(
        self,
        limit: int = 20,
        search: str | None = None,
        team_id: str | None = None,
        slug: str | None = None,
        from_: int | None = None,
        repo_url: str | None = None,
    ) -> dict[str, Any]:
        """List projects visible to the token."""
        params: dict[str, Any] = {
            "limit": limit,
            "search": search,
            "from": from_,
            "repoUrl": repo_url,
            **self._team_params(team_id, slug),
        }
        return self._request("/v9/projects", params)

    def get_project(
        self,
        project_id_or_name: str,
        team_id: str | None = None,
        slug: str | None = None,
    ) -> dict[str, Any]:
        """Get one project by id or name."""
        encoded = quote(project_id_or_name, safe="")
        return self._request(f"/v9/projects/{encoded}", self._team_params(team_id, slug))

    def list_deployments(
        self,
        limit: int = 20,
        app: str | None = None,
        project_id: str | None = None,
        target: str | None = None,
        state: str | None = None,
        team_id: str | None = None,
        slug: str | None = None,
        since: int | None = None,
        until: int | None = None,
        branch: str | None = None,
        sha: str | None = None,
    ) -> dict[str, Any]:
        """List deployments with optional project, target, state, branch, or SHA filters."""
        params: dict[str, Any] = {
            "limit": limit,
            "app": app,
            "projectId": project_id,
            "target": target,
            "state": state,
            "since": since,
            "until": until,
            "gitSource.ref": branch,
            "gitSource.sha": sha,
            **self._team_params(team_id, slug),
        }
        return self._request("/v6/deployments", params)

    def get_deployment(
        self,
        deployment_id_or_url: str,
        team_id: str | None = None,
        slug: str | None = None,
        with_git_repo_info: bool = True,
    ) -> dict[str, Any]:
        """Get a deployment by id or URL."""
        encoded = quote(deployment_id_or_url, safe="")
        params: dict[str, Any] = {
            "withGitRepoInfo": str(with_git_repo_info).lower(),
            **self._team_params(team_id, slug),
        }
        return self._request(f"/v13/deployments/{encoded}", params)

    def get_deployment_events(
        self,
        deployment_id_or_url: str,
        limit: int = 100,
        team_id: str | None = None,
        slug: str | None = None,
        since: int | None = None,
        until: int | None = None,
    ) -> dict[str, Any]:
        """Get recent build/runtime events for a deployment."""
        encoded = quote(deployment_id_or_url, safe="")
        params: dict[str, Any] = {
            "limit": limit,
            "since": since,
            "until": until,
            **self._team_params(team_id, slug),
        }
        return self._request(f"/v3/deployments/{encoded}/events", params)

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> VercelClient:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


def _client() -> VercelClient:
    return VercelClient()
