"""HubSpot CRM API client."""

from __future__ import annotations

from typing import Any

import httpx
from centaur_sdk import secret


class HubspotClient:
    """Authenticated HubSpot CRM client (OAuth or private app token)."""

    def __init__(self, access_token: str | None = None, timeout: float = 30.0):
        self._access_token_override = access_token
        self._timeout = timeout
        self._client: httpx.Client | None = None

    def _http(self) -> httpx.Client:
        if self._client is not None:
            return self._client

        token = self._access_token_override or secret("HUBSPOT_ACCESS_TOKEN", "")
        if not token:
            raise RuntimeError(
                "HUBSPOT_ACCESS_TOKEN not set.\n"
                "Grant a HubSpot OAuth wrapper secret to this principal, or set "
                "HUBSPOT_ACCESS_TOKEN for a private app token."
            )
        self._client = httpx.Client(
            base_url="https://api.hubapi.com",
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

    def __enter__(self) -> HubspotClient:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()

    def _request(self, method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        response = self._http().request(method, path, **kwargs)
        if response.status_code >= 400:
            try:
                error = response.json()
                msg = error.get("message") or error.get("error") or response.text
            except Exception:
                msg = response.text
            raise RuntimeError(f"HubSpot API error ({response.status_code}): {msg}")
        if not response.content:
            return {}
        parsed = response.json()
        return parsed if isinstance(parsed, dict) else {"value": parsed}

    def account_info(self) -> dict[str, Any]:
        """Return HubSpot account details for the current token (read-only)."""
        return self._request("GET", "/account-info/v3/details")

    def search_contacts(
        self,
        query: str,
        *,
        limit: int = 10,
        properties: list[str] | None = None,
    ) -> dict[str, Any]:
        """Search CRM contacts by free-text query."""
        props = properties or ["email", "firstname", "lastname", "company"]
        body = {
            "query": query,
            "limit": max(1, min(limit, 100)),
            "properties": props,
        }
        return self._request("POST", "/crm/v3/objects/contacts/search", json=body)

    def get_contact(self, contact_id: str, *, properties: list[str] | None = None) -> dict[str, Any]:
        """Fetch one contact by HubSpot object id."""
        params: dict[str, Any] = {}
        if properties:
            params["properties"] = ",".join(properties)
        return self._request("GET", f"/crm/v3/objects/contacts/{contact_id}", params=params)

    def search_companies(
        self,
        query: str,
        *,
        limit: int = 10,
        properties: list[str] | None = None,
    ) -> dict[str, Any]:
        """Search CRM companies by free-text query."""
        props = properties or ["name", "domain", "industry"]
        body = {
            "query": query,
            "limit": max(1, min(limit, 100)),
            "properties": props,
        }
        return self._request("POST", "/crm/v3/objects/companies/search", json=body)

    def raw_request(
        self,
        method: str,
        endpoint: str,
        json: dict[str, Any] | None = None,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Make a raw request to the HubSpot REST API (path under api.hubapi.com)."""
        path = endpoint if endpoint.startswith("/") else f"/{endpoint}"
        kwargs: dict[str, Any] = {}
        if json is not None:
            kwargs["json"] = json
        if params is not None:
            kwargs["params"] = params
        return self._request(method.upper(), path, **kwargs)


def _client() -> HubspotClient:
    return HubspotClient()
