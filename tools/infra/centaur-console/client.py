"""Client for centaur-console sandbox-scoped permission introspection."""

from __future__ import annotations

import os
from typing import Any

import httpx

SANDBOX_PERMISSIONS_PATH = "/api/v1/sandbox/permissions"


class ConsoleClient:
    """Read the current sandbox's redacted permissions from centaur-console."""

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
        # Optional local/debug override. In sandboxes, iron-proxy injects the
        # scoped Authorization header for this endpoint.
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

    def sandbox_permissions(self) -> dict[str, Any]:
        """Return the current sandbox's redacted permissions payload."""
        response = self.client.get(SANDBOX_PERMISSIONS_PATH)
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-console permissions request failed: {detail}") from exc

        payload = response.json()
        data = payload.get("data")
        if not isinstance(data, dict):
            raise RuntimeError("centaur-console permissions response did not include a data object")
        return data

    def permissions(self) -> dict[str, Any]:
        """Alias for tool bridge calls."""
        return self.sandbox_permissions()

    def health(self) -> dict[str, Any]:
        """Assert the sandbox permissions endpoint is reachable and authorized."""
        try:
            data = self.sandbox_permissions()
            return {
                "ok": True,
                "tool": "centaur-console",
                "error": None,
                "details": {
                    "sandbox_id": data.get("sandbox_id"),
                    "principal_id": data.get("principal_id"),
                    "proxy_id": data.get("proxy_id"),
                },
            }
        except Exception as exc:
            return {
                "ok": False,
                "tool": "centaur-console",
                "error": str(exc),
                "details": {},
            }

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> ConsoleClient:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


def _response_error_detail(response: httpx.Response) -> str:
    try:
        body = response.json()
    except ValueError:
        body = response.text
    return f"HTTP {response.status_code}: {body}"


def _client() -> ConsoleClient:
    return ConsoleClient()
