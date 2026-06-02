"""Messari API client."""

from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL_V1 = "https://api.messari.io/metrics/v1"
BASE_URL_V2 = "https://api.messari.io/metrics/v2"


class MessariClient:
    """Client for Messari API."""

    def __init__(self, api_key: str | None = None):
        self._api_key = api_key or secret("MESSARI_API_KEY", "")
        if not self._api_key:
            raise RuntimeError("MESSARI_API_KEY not set.")
        self._client = httpx.Client(
            headers={"x-messari-api-key": self._api_key},
            timeout=30.0,
        )

    def _request(
        self, endpoint: str, version: int = 1, params: dict | None = None
    ) -> dict[str, Any]:
        """Make request to Messari API."""
        base_url = BASE_URL_V1 if version == 1 else BASE_URL_V2
        url = f"{base_url}{endpoint}"
        response = self._client.get(url, params=params)
        if response.status_code >= 400:
            try:
                error = response.json()
                msg = error.get("status", {}).get("error_message", response.text)
            except Exception:
                msg = response.text
            raise RuntimeError(f"Messari API error ({response.status_code}): {msg}")
        return response.json()

    def list_assets(self, asset_key: str = "bitcoin", limit: int = 20) -> list[dict]:
        """List assets from the current Messari Metrics API."""
        _ = asset_key
        data = self._request("/assets", version=1, params={"limit": limit})
        result = data.get("data", [])
        return result if isinstance(result, list) else []

    def get_asset(self, asset_key: str) -> dict:
        """Get asset details by slug or ID."""
        data = self._request(f"/assets/{asset_key}", version=1)
        return data.get("data", {})

    def get_asset_metrics(self, asset_key: str) -> dict:
        """Get metrics for an asset."""
        data = self._request(f"/assets/{asset_key}", version=1)
        return data.get("data", {})

    def get_asset_profile(self, asset_key: str) -> dict:
        """Get asset details; profile fields depend on Messari subscription tier."""
        data = self._request(f"/assets/{asset_key}", version=1)
        return data.get("data", {})

    def get_asset_markets(self, asset_key: str) -> dict:
        """Get asset details; market fields depend on Messari subscription tier."""
        data = self._request(f"/assets/{asset_key}", version=1)
        return data.get("data", {})

    def get_news(self, limit: int = 10) -> dict:
        """Get asset metrics for bitcoin (news endpoint deprecated).

        Note: The /news endpoint has been disabled on data.messari.io.
        Returns bitcoin metrics as a fallback.
        """
        data = self._request("/assets/bitcoin", version=1)
        return data.get("data", {})

    def get_timeseries(
        self,
        asset_key: str,
        metric: str,
        start: str | None = None,
        end: str | None = None,
    ) -> dict:
        """Get timeseries data for an asset metric."""
        params: dict[str, Any] = {}
        if start:
            params["start"] = start
        if end:
            params["end"] = end
        data = self._request(
            f"/assets/{asset_key}/metrics/{metric}/time-series", version=1, params=params
        )
        return data.get("data", {})

    def raw_request(self, endpoint: str, version: int = 1, params: dict | None = None) -> dict:
        """Make a raw API call to any endpoint."""
        return self._request(endpoint, version=version, params=params)

    def close(self):
        self._client.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> MessariClient:
    return MessariClient()
