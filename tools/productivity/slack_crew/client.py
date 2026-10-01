"""Self-management through the proxy-injected sandbox identity, never admin auth."""

import os
from typing import Any

import httpx


class SlackCrewClient:
    def __init__(self, base_url: str | None = None, transport: httpx.BaseTransport | None = None):
        # Non-secret service discovery, also used by the other sandbox Console tools.
        self.base_url = (
            base_url or os.getenv("CENTAUR_CONSOLE_URL", "http://centaur-console:3000")  # noqa: TID251
        ).rstrip("/")
        self.transport = transport

    def _request(self, method: str, body: dict | None = None) -> dict[str, Any]:
        # iron-proxy injects a short-lived sandbox entitlement only for this path.
        # The Console derives the app from the current durable sandbox assignment.
        with httpx.Client(timeout=35, follow_redirects=False, transport=self.transport) as client:
            response = client.request(
                method,
                f"{self.base_url}/api/v1/sandbox/crew/me",
                json={"data": body} if body is not None else None,
                headers={"Accept": "application/json"},
            )
        if response.status_code != 200:
            raise RuntimeError(
                f"Crew self-management returned HTTP {response.status_code}; only an active Crew sandbox can edit itself"
            )
        return response.json()["data"]

    def me(self) -> dict[str, Any]:
        """Read this bot's profile, without credentials or access to other bots."""
        return self._request("GET")

    def edit(self, name: str | None = None, description: str | None = None) -> dict[str, Any]:
        """Change only this bot's Slack display name or short description."""
        fields = {
            key: value
            for key, value in {"name": name, "description": description}.items()
            if value is not None
        }
        if not fields:
            raise ValueError("Provide a name or description")
        return self._request("PATCH", fields)


def _client() -> SlackCrewClient:
    return SlackCrewClient()
