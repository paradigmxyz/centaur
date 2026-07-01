"""EODHD Financial API client — real-time quotes and historical EOD prices."""

from typing import Any

import httpx

from centaur_sdk.tool_sdk import secret

BASE_URL = "https://eodhd.com/api"


class EodhdClient:
    def __init__(self, api_key: str | None = None):
        self._api_key = api_key or secret("EODHD_API_KEY", "")
        if not self._api_key:
            raise RuntimeError(
                "EODHD API key not set.\nRequired: EODHD_API_KEY\nGet a key at https://eodhd.com/"
            )

    def _request(self, path: str, params: dict[str, Any] | None = None) -> Any:
        """Make an authenticated GET request to the EODHD API."""
        query: dict[str, Any] = {
            "api_token": self._api_key,
            "fmt": "json",
        }
        if params:
            query.update(params)

        with httpx.Client(base_url=BASE_URL, timeout=30.0) as client:
            response = client.get(path, params=query)
            if response.status_code >= 400:
                try:
                    error = response.json()
                    msg = error.get("message", error.get("error", response.text))
                except Exception:
                    msg = response.text
                raise RuntimeError(f"EODHD API error ({response.status_code}): {msg}")
            return response.json()

    def get_quote(self, symbol: str) -> dict[str, Any]:
        """Get a delayed quote for a US equity (open, high, low, close, volume, change %).

        Args:
            symbol: Ticker symbol (e.g. "AAPL"). The .US suffix is appended automatically.
        """
        return self._request(f"/real-time/{symbol}.US")

    def get_eod_prices(
        self,
        symbol: str,
        from_date: str | None = None,
        to_date: str | None = None,
    ) -> list[dict[str, Any]]:
        """Get historical daily OHLCV prices for a US equity.

        Args:
            symbol: Ticker symbol (e.g. "AAPL"). The .US suffix is appended automatically.
            from_date: Start date in YYYY-MM-DD format.
            to_date: End date in YYYY-MM-DD format.

        Returns:
            List of dicts with date, open, high, low, close, adjusted_close, volume.
        """
        params: dict[str, Any] = {}
        if from_date:
            params["from"] = from_date
        if to_date:
            params["to"] = to_date
        return self._request(f"/eod/{symbol}.US", params=params)


def _client() -> EodhdClient:
    return EodhdClient(api_key=secret("EODHD_API_KEY"))
