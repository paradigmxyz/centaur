"""Artemis market data client."""

from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://data-svc.artemisxyz.com"
MARKET_DATA_ENDPOINT = "/data/api/"
METRICS = ["PRICE", "24H_PRICE_CHG_PCT"]


class ArtemisClient:
    """Client for Artemis market data."""

    def __init__(self, api_key: str | None = None, timeout: float = 30.0):
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            api_key = self._api_key or secret("ARTEMIS_API_KEY", "")
            headers = {"X-API-Key": api_key} if api_key else {}

            self._client = httpx.Client(
                base_url=BASE_URL,
                timeout=self.timeout,
                follow_redirects=False,
                headers=headers,
            )
        return self._client

    def _request(self, path: str, params: dict[str, str]) -> dict[str, Any]:
        response = self.client.get(path, params=params)
        response.raise_for_status()
        return response.json()

    def get_market_data(self, symbols: list[str]) -> dict[str, Any]:
        """Return Artemis's data.symbols response for symbols, e.g. BTC, ETH.

        Symbols are passed unchanged. PRICE and 24H_PRICE_CHG_PCT retain their
        upstream values, including fractional changes, nulls, and error strings.
        """
        metric_args = ",".join(METRICS)
        path = f"{MARKET_DATA_ENDPOINT}{metric_args}/"
        params = {"symbols": ",".join(symbols)}

        return self._request(path, params)

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> "ArtemisClient":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _client() -> ArtemisClient:
    return ArtemisClient()
