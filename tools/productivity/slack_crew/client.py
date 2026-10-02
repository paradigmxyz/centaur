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

    def _request(
        self, method: str, body: dict | None = None, *, path: str = "me", params: dict | None = None
    ) -> dict[str, Any]:
        # iron-proxy injects a short-lived sandbox entitlement only for this path.
        # The Console derives the app from the current durable sandbox assignment.
        with httpx.Client(timeout=35, follow_redirects=False, transport=self.transport) as client:
            response = client.request(
                method,
                f"{self.base_url}/api/v1/sandbox/crew/{path}",
                json={"data": body} if body is not None else None,
                params=params,
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

    def memories(self, query: str = "", include_expired: bool = False) -> dict[str, Any]:
        """Recall only this conversation's and this bot's explicitly shared memories."""
        return self._request(
            "GET",
            path="memories",
            params={"q": query, "include_expired": str(include_expired).lower()},
        )

    def remember(
        self,
        key: str,
        content: str,
        *,
        shared: bool = False,
        source: str | None = None,
        expires_at: str | None = None,
    ) -> dict[str, Any]:
        scope = "shared" if shared else "conversation"
        current = next(
            (
                m
                for m in self.memories(include_expired=True)["memories"]
                if m["key"] == key and m["scope"] == scope
            ),
            None,
        )
        fields = {"key": key, "scope": scope, "content": content}
        if current is not None:
            fields["lock_version"] = current["lock_version"]
        if source is not None:
            fields["source"] = source
        if expires_at is not None:
            fields["expires_at"] = expires_at
        return self._request("PUT", fields, path="memories")

    def forget(self, key: str, *, shared: bool = False) -> dict[str, Any]:
        scope = "shared" if shared else "conversation"
        current = next(
            (
                m
                for m in self.memories(include_expired=True)["memories"]
                if m["key"] == key and m["scope"] == scope
            ),
            None,
        )
        if current is None:
            raise ValueError("No memory with that key in the selected scope")
        return self._request(
            "DELETE",
            {"key": key, "scope": scope, "lock_version": current["lock_version"]},
            path="memories",
        )

    def history(self, version: int | None = None) -> dict[str, Any]:
        return self._request(
            "GET", path="history", params={"version": version} if version is not None else None
        )

    def restore(self, version: int) -> dict[str, Any]:
        current = self.me()
        return self._request(
            "POST", {"version": version, "lock_version": current["lock_version"]}, path="restore"
        )

    def edit(
        self,
        name: str | None = None,
        description: str | None = None,
        *,
        icon_url: str | None = None,
        system_prompt: str | None = None,
        skills: list[dict[str, str]] | None = None,
        codex_model: str | None = None,
        claude_model: str | None = None,
    ) -> dict[str, Any]:
        """Edit this bot's behavior; role grants and other bots are inaccessible."""
        fields = {
            key: value
            for key, value in {
                "name": name,
                "description": description,
                "icon_url": icon_url,
                "system_prompt": system_prompt,
                "skills": skills,
            }.items()
            if value is not None
        }
        if (
            system_prompt is not None
            or skills is not None
            or codex_model is not None
            or claude_model is not None
        ):
            current = self.me()
            fields["lock_version"] = current["lock_version"]
            if codex_model is not None or claude_model is not None:
                models = dict(current["default_models"])
                if codex_model is not None:
                    models["codex"] = codex_model
                if claude_model is not None:
                    models["claude"] = claude_model
                fields["default_models"] = models
        if not fields:
            raise ValueError("Provide at least one field to edit")
        return self._request("PATCH", fields)


def _client() -> SlackCrewClient:
    return SlackCrewClient()
