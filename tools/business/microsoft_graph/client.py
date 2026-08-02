"""Microsoft Graph API client (delegated user token)."""

from __future__ import annotations

from typing import Any

import httpx
from centaur_sdk import secret


class MicrosoftGraphClient:
    """Authenticated Microsoft Graph client for delegated (user) access."""

    def __init__(self, access_token: str | None = None, timeout: float = 30.0):
        self._access_token_override = access_token
        self._timeout = timeout
        self._client: httpx.Client | None = None

    def _http(self) -> httpx.Client:
        if self._client is not None:
            return self._client

        token = self._access_token_override or secret("MICROSOFT_GRAPH_TOKEN", "")
        if not token:
            raise RuntimeError(
                "MICROSOFT_GRAPH_TOKEN not set.\n"
                "Grant a Microsoft OAuth wrapper secret to this principal, or set "
                "MICROSOFT_GRAPH_TOKEN for local use."
            )
        self._client = httpx.Client(
            base_url="https://graph.microsoft.com/v1.0",
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            timeout=self._timeout,
        )
        return self._client

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> MicrosoftGraphClient:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()

    def _request(self, method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        response = self._http().request(method, path, **kwargs)
        if response.status_code >= 400:
            try:
                error = response.json()
                msg = (
                    (error.get("error") or {}).get("message")
                    if isinstance(error.get("error"), dict)
                    else error.get("error")
                ) or response.text
            except Exception:
                msg = response.text
            raise RuntimeError(f"Microsoft Graph error ({response.status_code}): {msg}")
        if not response.content:
            return {}
        parsed = response.json()
        return parsed if isinstance(parsed, dict) else {"value": parsed}

    def me(self) -> dict[str, Any]:
        """Return the signed-in user's profile."""
        return self._request("GET", "/me")

    def list_messages(
        self,
        *,
        top: int = 10,
        search: str | None = None,
        select: list[str] | None = None,
    ) -> dict[str, Any]:
        """List messages in the signed-in user's mailbox (Mail.Read)."""
        params: dict[str, Any] = {"$top": max(1, min(top, 50))}
        if search:
            params["$search"] = f'"{search}"'
        if select:
            params["$select"] = ",".join(select)
        else:
            params["$select"] = "id,subject,from,receivedDateTime,isRead"
        headers = {"ConsistencyLevel": "eventual"} if search else None
        return self._request("GET", "/me/messages", params=params, headers=headers)

    def list_events(
        self,
        *,
        top: int = 10,
        select: list[str] | None = None,
    ) -> dict[str, Any]:
        """List upcoming calendar events (Calendars.Read)."""
        params: dict[str, Any] = {"$top": max(1, min(top, 50))}
        if select:
            params["$select"] = ",".join(select)
        else:
            params["$select"] = "id,subject,start,end,organizer"
        return self._request("GET", "/me/events", params=params)

    def raw_request(
        self,
        method: str,
        endpoint: str,
        json: dict[str, Any] | None = None,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Make a raw request under graph.microsoft.com/v1.0."""
        path = endpoint if endpoint.startswith("/") else f"/{endpoint}"
        if path.startswith("/v1.0"):
            path = path[len("/v1.0") :]
        kwargs: dict[str, Any] = {}
        if json is not None:
            kwargs["json"] = json
        if params is not None:
            kwargs["params"] = params
        return self._request(method.upper(), path, **kwargs)


def _client() -> MicrosoftGraphClient:
    return MicrosoftGraphClient()
