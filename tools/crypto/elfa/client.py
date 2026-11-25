"""Elfa AI event summary client.

API reference: https://docs.elfa.ai/api/rest/get-event-summary-v-2/
"""

from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://api.elfa.ai"
DEFAULT_TIMEOUT_SECONDS = 180.0
DEFAULT_TIME_WINDOW = "24h"


class ElfaClient:
    """Client for the Elfa AI API."""

    def __init__(
        self, api_key: str | None = None, timeout: float = DEFAULT_TIMEOUT_SECONDS
    ) -> None:
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _request(self, path: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        headers = {"x-elfa-api-key": self._api_key or secret("ELFA_API_KEY")}

        response = self.client.get(f"{BASE_URL}{path}", params=params, headers=headers)
        response.raise_for_status()

        return response.json()

    def get_event_summary(
        self,
        keywords: str,
        time_window: str = DEFAULT_TIME_WINDOW,
    ) -> dict[str, Any]:
        """Summarize recent keyword mentions with source links.

        Keywords can be a ticker such as "ETH" or comma-separated alternatives.
        The lookback defaults to 24h; use e.g. "1h" or "7d" to change it.
        Returns the full response, which may contain multiple event summaries.
        """
        params = {"keywords": keywords, "timeWindow": time_window}

        return self._request("/v2/data/event-summary", params=params)

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> "ElfaClient":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _client() -> ElfaClient:
    return ElfaClient()
