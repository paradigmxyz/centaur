"""Preqin API client."""

from __future__ import annotations

from typing import Any

import httpx

from centaur_sdk import secret


OPERATIONAL_BASE_URL = "https://api.preqin.com"
IDENTITY_BASE_URL = "https://id.preqin.com"
FEEDS_BASE_URL = "https://feeds.preqin.com"
OPERATIONAL_TOKEN_PLACEHOLDER = "PREQIN_OPERATIONAL_TOKEN"


def _clean_secret(value: str | None) -> str | None:
    """Return the first non-empty secret line without exposing secret contents."""
    if not value:
        return None
    stripped = value.strip()
    if "\n" not in stripped:
        return stripped or None
    for line in stripped.splitlines():
        candidate = line.strip()
        if candidate and not candidate.startswith(("===", "#")):
            return candidate
    return None


def _redact_token_payload(payload: dict[str, Any]) -> dict[str, Any]:
    """Redact token-bearing fields for diagnostics."""
    return {
        key: "<redacted>" if "token" in key.casefold() or "secret" in key.casefold() else value
        for key, value in payload.items()
    }


def _credential_present(name: str, value: str | None) -> bool:
    return bool(value) and value != name


class PreqinClient:
    """Client for Preqin Operational API and Feeds API."""

    def __init__(
        self,
        username: str | None = None,
        api_key: str | None = None,
        timeout: float = 30.0,
    ):
        self._username = _clean_secret(username)
        self._api_key = _clean_secret(api_key)
        self.timeout = timeout
        self._client: httpx.Client | None = None
        self._operational_token: str | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _username_value(self) -> str | None:
        return self._username or _clean_secret(secret("PREQIN_USERNAME", ""))

    def _api_key_value(self) -> str | None:
        return self._api_key or _clean_secret(secret("PREQIN_API_KEY", ""))

    def credential_status(self) -> dict[str, Any]:
        """Report whether required Preqin secret names resolve, without exposing values."""
        fields = {
            "PREQIN_USERNAME": self._username_value(),
            "PREQIN_API_KEY": self._api_key_value(),
        }
        return {
            name: {
                "present": _credential_present(name, value),
                "length": len(value or ""),
            }
            for name, value in fields.items()
        }

    def _operational_access_token(self, force_refresh: bool = False) -> str:
        """Acquire a bearer token from Preqin's Operational API token endpoint."""
        if self._operational_token and not force_refresh:
            return self._operational_token

        username = self._username_value()
        api_key = self._api_key_value()
        if not username:
            raise RuntimeError("PREQIN_USERNAME not set.")
        if not api_key:
            raise RuntimeError("PREQIN_API_KEY not set.")

        response = self.client.post(
            f"{OPERATIONAL_BASE_URL}/connect/token",
            files={
                "username": (None, username),
                "apikey": (None, api_key),
            },
            headers={"Accept": "application/json"},
        )
        if response.status_code >= 400:
            body = response.text.strip()
            detail = f" - {body}" if body else ""
            raise RuntimeError(
                "Preqin Operational API auth failed "
                f"({response.status_code}) at /connect/token using username/api key{detail}"
            )

        data = response.json()
        token = data.get("access_token") or data.get("accessToken") or data.get("token")
        if not token:
            raise RuntimeError(
                "Preqin Operational API auth response did not include an access token: "
                f"{_redact_token_payload(data)}"
            )
        self._operational_token = token
        return token

    def auth_health(self) -> dict[str, Any]:
        """Check Preqin auth and return a redacted diagnostic result."""
        url = f"{OPERATIONAL_BASE_URL}/api/FundManager"
        try:
            data = self._operational_get("/api/FundManager", {"Size": 1, "Page": 1})
            return {
                "ok": True,
                "url": url,
                "method": "operational_get",
                "records_seen": len(data) if isinstance(data, list) else None,
            }
        except Exception as exc:
            return {
                "ok": False,
                "url": url,
                "method": "operational_get",
                "error": str(exc),
                "credentials": self.credential_status(),
            }

    def _operational_get(self, endpoint: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        token = self._operational_token or _clean_secret(secret(OPERATIONAL_TOKEN_PLACEHOLDER, ""))
        username = self._username_value()
        api_key = self._api_key_value()
        if not token and _credential_present("PREQIN_USERNAME", username) and _credential_present(
            "PREQIN_API_KEY", api_key
        ):
            token = self._operational_access_token()
        headers = {"Accept": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        response = self.client.get(
            f"{OPERATIONAL_BASE_URL}{endpoint}",
            params={key: value for key, value in (params or {}).items() if value is not None},
            headers=headers,
        )
        if response.status_code >= 400:
            body = response.text.strip()
            detail = f" - {body}" if body else ""
            raise RuntimeError(f"Preqin API error ({response.status_code}) for {endpoint}{detail}")
        return response.json()

    def get_fund_managers(
        self,
        fund_manager_name: str | None = None,
        fund_manager_id: str | None = None,
        asset_class: str | None = None,
        include: str | None = None,
        size: int = 20,
        page: int = 1,
    ) -> dict[str, Any]:
        """Search Preqin fund managers."""
        return self._operational_get(
            "/api/FundManager",
            {
                "FundManagerName": fund_manager_name,
                "FundManagerID": fund_manager_id,
                "AssetClass": asset_class,
                "Include": include,
                "Size": size,
                "Page": page,
            },
        )

    def get_funds(
        self,
        fund_name: str | None = None,
        fund_id: str | None = None,
        fund_manager_name: str | None = None,
        fund_manager_id: str | None = None,
        asset_class: str | None = None,
        strategy: str | None = None,
        status: str | None = None,
        include: str | None = None,
        size: int = 20,
        page: int = 1,
    ) -> dict[str, Any]:
        """Search Preqin funds."""
        return self._operational_get(
            "/api/Fund",
            {
                "FundName": fund_name,
                "FundId": fund_id,
                "FundManagerName": fund_manager_name,
                "FundManagerId": fund_manager_id,
                "AssetClass": asset_class,
                "Strategy": strategy,
                "Status": status,
                "Include": include,
                "Size": size,
                "Page": page,
            },
        )

    def find_paradigm_xyz(self, size: int = 20) -> dict[str, Any]:
        """Find the best matching Preqin fund-manager and fund records for Paradigm XYZ."""
        manager_matches = self.get_fund_managers(fund_manager_name="Paradigm XYZ", size=size)
        fund_matches = self.get_funds(fund_manager_name="Paradigm XYZ", size=size)
        fallback_fund_matches = self.get_funds(fund_name="Paradigm", size=size)
        return {
            "query": "Paradigm XYZ",
            "fund_managers": manager_matches,
            "funds_by_manager": fund_matches,
            "funds_by_name_fallback": fallback_fund_matches,
        }

    def list_feed_specs(self) -> list[dict[str, Any]]:
        """List public Preqin Feeds API OpenAPI spec versions."""
        response = self.client.get(f"{FEEDS_BASE_URL}/OpenApiSpecs", headers={"Accept": "application/json"})
        response.raise_for_status()
        return response.json()

    def close(self):
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> PreqinClient:
    return PreqinClient()
