"""NewsAPI.org client."""

import subprocess

import httpx

from centaur_sdk import secret


class NewsAPIClient:
    """Client for NewsAPI.org."""

    BASE_URL = "https://newsapi.org/v2"

    def __init__(self, api_key: str | None = None, timeout: float = 30.0):
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _get_api_key(self) -> str | None:
        """Get API key from instance, env var, or 1Password."""
        if self._api_key:
            return self._api_key

        key = secret("NEWSAPI_KEY", "")
        if key:
            return key

        try:
            result = subprocess.run(
                ["op", "read", "op://ai-agents/NewsAPI Key/credential"],
                capture_output=True,
                text=True,
                timeout=10,
            )
            if result.returncode == 0:
                return result.stdout.strip()
        except (subprocess.TimeoutExpired, FileNotFoundError):
            pass

        return None

    def _request(self, endpoint: str, params: dict | None = None) -> dict:
        """Make an API request."""
        api_key = self._get_api_key()
        if not api_key:
            raise RuntimeError("NEWSAPI_KEY not set. Set env var or use 1Password.")

        url = f"{self.BASE_URL}{endpoint}"
        headers = {"X-Api-Key": api_key}

        try:
            response = self.client.get(url, params=params, headers=headers)
            response.raise_for_status()
            data = response.json()
            if data.get("status") == "error":
                raise RuntimeError(f"API error: {data.get('message', 'Unknown error')}")
            return data
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"API error: {e.response.status_code} - {e.response.text}")
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}")

    def headlines(
        self,
        country: str | None = None,
        category: str | None = None,
        sources: str | None = None,
        q: str | None = None,
        page_size: int = 20,
        page: int = 1,
    ) -> dict:
        """Get top headlines."""
        params = {"pageSize": page_size, "page": page}
        if country:
            params["country"] = country
        if category:
            params["category"] = category
        if sources:
            params["sources"] = sources
        if q:
            params["q"] = q
        return self._request("/top-headlines", params=params)

    def search(
        self,
        q: str,
        sources: str | None = None,
        domains: str | None = None,
        from_date: str | None = None,
        to_date: str | None = None,
        language: str | None = None,
        sort_by: str = "publishedAt",
        page_size: int = 20,
        page: int = 1,
    ) -> dict:
        """Search all articles."""
        params = {
            "q": q,
            "sortBy": sort_by,
            "pageSize": page_size,
            "page": page,
        }
        if sources:
            params["sources"] = sources
        if domains:
            params["domains"] = domains
        if from_date:
            params["from"] = from_date
        if to_date:
            params["to"] = to_date
        if language:
            params["language"] = language
        return self._request("/everything", params=params)

    def sources(
        self,
        category: str | None = None,
        language: str | None = None,
        country: str | None = None,
    ) -> dict:
        """Get available news sources."""
        params = {}
        if category:
            params["category"] = category
        if language:
            params["language"] = language
        if country:
            params["country"] = country
        return self._request("/top-headlines/sources", params=params)

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> NewsAPIClient:
    return NewsAPIClient()
