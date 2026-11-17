"""Client for Browser Use's cloud browser, LLM gateway and search APIs.

API reference: https://docs.browser-use.com. Page-level control (navigate,
HTML, click, ...) goes over CDP and lives in the Code Mode ``browser_use``
connector; this client covers the plain HTTP endpoints.
"""

from __future__ import annotations

from typing import Any
from urllib.parse import quote

import httpx

from centaur_sdk import secret

BASE_URL = "https://api.browser-use.com"
BROWSERS = "/api/v4/browsers"
LLM = "/api/v4/llm/v1"
SEARCH = "/api/v4/search"
DEFAULT_MODEL = "bu-4.0"


class BrowserUseError(RuntimeError):
    """A Browser Use API error, without the request's credentials."""


class BrowserUseClient:
    """Client for Browser Use's HTTP APIs."""

    def __init__(self, api_key: str | None = None, timeout: float = 120.0) -> None:
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(base_url=BASE_URL, timeout=self.timeout)
        return self._client

    @property
    def headers(self) -> dict[str, str]:
        return {"X-Browser-Use-API-Key": self._api_key or secret("BROWSER_USE_API_KEY")}

    def _request(
        self, method: str, path: str, *, json: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        response = self.client.request(method, path, headers=self.headers, json=json)
        if response.is_error:
            raise BrowserUseError(
                f"Browser Use returned HTTP {response.status_code}: {_detail(response)}"
            )
        result = response.json()
        if not isinstance(result, dict):
            raise BrowserUseError("Browser Use returned an unexpected response.")
        return result

    # Browsers. The CDP URL grants control of a browser without the key, so
    # it is never returned.

    def create_browser(
        self,
        *,
        timeout_minutes: int = 15,
        profile_id: str | None = None,
        proxy_country_code: str | None = None,
        network_policy_id: str | None = None,
    ) -> dict[str, Any]:
        """Start a cloud browser; it bills until stopped or timed out."""
        body: dict[str, Any] = {"timeout": timeout_minutes}
        if profile_id:
            body["profileId"] = profile_id
        if proxy_country_code:
            body["proxyCountryCode"] = proxy_country_code
        if network_policy_id:
            body["networkPolicyId"] = network_policy_id
        return _public(self._request("POST", BROWSERS, json=body))

    def get_browser(self, browser_id: str) -> dict[str, Any]:
        """Return a browser's state and costs."""
        return _public(self._request("GET", f"{BROWSERS}/{quote(browser_id, safe='')}"))

    def stop_browser(self, browser_id: str) -> dict[str, Any]:
        """Stop a browser, ending its billing."""
        path = f"{BROWSERS}/{quote(browser_id, safe='')}"
        return _public(self._request("PATCH", path, json={"action": "stop"}))

    # LLM gateway (OpenAI Responses API) and search.

    def list_models(self) -> dict[str, Any]:
        """Return the models the LLM gateway serves to this key (free)."""
        return self._request("GET", f"{LLM}/models")

    def respond(
        self,
        input: str,
        *,
        model: str = DEFAULT_MODEL,
        instructions: str | None = None,
        max_output_tokens: int | None = None,
        reasoning_effort: str | None = None,
    ) -> dict[str, Any]:
        """Send one Responses API request; the gateway stores nothing."""
        body: dict[str, Any] = {"model": model, "input": input, "store": False, "stream": False}
        if instructions:
            body["instructions"] = instructions
        if max_output_tokens is not None:
            body["max_output_tokens"] = max_output_tokens
        if reasoning_effort:
            body["reasoning"] = {"effort": reasoning_effort}
        response = self._request("POST", f"{LLM}/responses", json=body)
        if response.get("status") == "failed" or response.get("error"):
            raise BrowserUseError(f"Browser Use LLM request failed: {response.get('error')}")
        return response

    def search(
        self, query: str, *, num_results: int | None = None, category: str | None = None
    ) -> dict[str, Any]:
        """Search the web with Browser Use (charged per search)."""
        body: dict[str, Any] = {"query": query}
        if num_results is not None:
            body["num_results"] = num_results
        if category:
            body["category"] = category
        return self._request("POST", SEARCH, json=body)

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> BrowserUseClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def output_text(response: dict[str, Any]) -> str:
    """The concatenated output text of a Responses API response."""
    parts = []
    for item in response.get("output") or []:
        if isinstance(item, dict) and item.get("type") == "message":
            for content in item.get("content") or []:
                if isinstance(content, dict) and content.get("type") == "output_text":
                    parts.append(str(content.get("text", "")))
    return "".join(parts)


def _public(browser: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in browser.items() if key != "cdpUrl"}


def _detail(response: httpx.Response) -> str:
    try:
        payload = response.json()
    except ValueError:
        return response.text[:300]
    if isinstance(payload, dict):
        error = payload.get("error")
        if isinstance(error, dict) and (error.get("message") or error.get("code")):
            return str(error.get("message") or error.get("code"))[:300]
        if payload.get("detail") is not None:
            return str(payload["detail"])[:300]
    return response.text[:300]


def _client() -> BrowserUseClient:
    return BrowserUseClient()
