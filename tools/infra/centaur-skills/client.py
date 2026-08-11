"""Client for the sandbox-scoped Console skill catalog."""

from __future__ import annotations

import os
from typing import Any
from urllib.parse import quote

import httpx

SANDBOX_SKILLS_PATH = "/api/v1/sandbox/skills"


class SkillsClient:
    """Read Console skills visible to the current sandbox principal."""

    def __init__(
        self,
        url: str | None = None,
        bearer_token: str | None = None,
        timeout: float = 30.0,
        transport: httpx.BaseTransport | None = None,
    ):
        self._url = url
        self._bearer_token = bearer_token
        self.timeout = timeout
        self._transport = transport
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        # Non-secret endpoint config. Sandboxes receive this from api-rs.
        url = (self._url or os.getenv("CENTAUR_CONSOLE_URL", "http://centaur-console:3000")).strip().rstrip("/")  # noqa: TID251
        if url and not url.startswith(("http://", "https://")):
            url = f"http://{url}"
        return url

    def _headers(self) -> dict[str, str]:
        headers = {"Accept": "application/json"}
        # iron-proxy injects this entitlement in sandboxes. The environment
        # override exists only for local debugging.
        bearer = (self._bearer_token or os.getenv("CENTAUR_CONSOLE_BEARER_TOKEN", "")).strip()  # noqa: TID251
        if bearer:
            headers["Authorization"] = f"Bearer {bearer}"
        return headers

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(
                base_url=self.base_url,
                headers=self._headers(),
                timeout=self.timeout,
                transport=self._transport,
            )
        return self._client

    def list(self, scope: str | None = None, limit: int = 20) -> list[dict[str, Any]]:
        """List Console skills visible to the current sandbox principal."""
        params: dict[str, str | int] = {"limit": limit}
        if scope:
            params["scope"] = scope
        result = self._request(SANDBOX_SKILLS_PATH, params=params)
        if not isinstance(result, list):
            raise RuntimeError("centaur-skills response did not include a data array")
        return result

    def search(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        """Search Console skills visible to the current sandbox principal."""
        result = self._request(
            f"{SANDBOX_SKILLS_PATH}/search",
            params={"q": query, "limit": limit},
        )
        if not isinstance(result, list):
            raise RuntimeError("centaur-skills response did not include a data array")
        return result

    def read(self, identifier: str) -> dict[str, Any]:
        """Read one visible Console skill by exact name or OID."""
        result = self._request(f"{SANDBOX_SKILLS_PATH}/{quote(identifier, safe='')}")
        if not isinstance(result, dict):
            raise RuntimeError("centaur-skills response did not include a data object")
        return result

    def _request(
        self,
        path: str,
        params: dict[str, str | int] | None = None,
    ) -> dict[str, Any] | list[dict[str, Any]]:
        response = self.client.get(path, params=params)
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-skills request failed: {detail}") from exc
        except httpx.RequestError as exc:
            raise RuntimeError(f"centaur-skills request failed: {exc}") from exc

        payload = response.json()
        data = payload.get("data")
        if not isinstance(data, (dict, list)):
            raise RuntimeError("centaur-skills response did not include data")
        return data

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> SkillsClient:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


def _response_error_detail(response: httpx.Response) -> str:
    try:
        body = response.json()
    except ValueError:
        body = response.text
    return f"HTTP {response.status_code}: {body}"


def _client() -> SkillsClient:
    return SkillsClient()
