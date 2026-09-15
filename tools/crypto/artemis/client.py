"""Artemis market data client."""

from typing import Any

from artemis import Artemis, DefaultHttpxClient, omit
from centaur_sdk import secret

BASE_URL = "https://data-svc.artemisxyz.com"
DEFAULT_TIMEOUT = 30.0
MAX_RETRIES = 2
API_KEY_SECRET = "ARTEMIS_API_KEY"
API_KEY_HEADER = "X-API-Key"
MARKET_DATA_ENDPOINT = "/data/api/"
METRICS = ["PRICE", "24H_PRICE_CHG_PCT"]


class ArtemisClient:
    """Client for Artemis market data."""

    def __init__(self, api_key: str | None = None, timeout: float = DEFAULT_TIMEOUT):
        self._api_key = api_key
        self.timeout = timeout
        self._client: Artemis | None = None

    @property
    def client(self) -> Artemis:
        if self._client is None:
            self._client = Artemis(
                api_key="",
                base_url=BASE_URL,
                timeout=self.timeout,
                max_retries=MAX_RETRIES,
                http_client=DefaultHttpxClient(follow_redirects=False),
            )
        return self._client

    def _request(self, symbols: list[str]) -> dict[str, Any]:
        endpoint = f"{MARKET_DATA_ENDPOINT}{','.join(METRICS)}/"
        api_key = self._api_key or secret(API_KEY_SECRET, "")
        params = {"symbols": ",".join(symbols)}
        headers = {API_KEY_HEADER: api_key or omit}

        # Uses SDK get() because fetch_metrics() requires start_date/end_date.
        return self.client.get(
            endpoint,
            cast_to=dict[str, Any],
            options={"params": params, "headers": headers},
        )

    def get_market_data(self, symbols: list[str]) -> dict[str, Any]:
        """Return Artemis's data.symbols response for symbols, e.g. BTC, ETH.

        Symbols are passed unchanged. PRICE and 24H_PRICE_CHG_PCT retain their
        upstream values, including fractional changes, nulls, and error strings.
        """
        return self._request(symbols)

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
