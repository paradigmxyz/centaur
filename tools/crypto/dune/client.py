"""Dune Analytics API client."""

from typing import Any

import httpx


class DuneClient:
    """Dune Analytics API client."""

    def __init__(self, api_key: str | None = None):
        self._api_key = api_key or ""
        if not self._api_key:
            raise RuntimeError(
                "DUNE_API_KEY not set.\nGet your API key at https://dune.com/settings/api"
            )
        self._client = httpx.Client(
            base_url="https://api.dune.com/api/v1",
            headers={
                "X-Dune-API-Key": self._api_key,
                "Content-Type": "application/json",
            },
            timeout=60.0,
        )

    def _request(self, method: str, path: str, **kwargs) -> dict[str, Any]:
        """Make authenticated request to Dune API."""
        response = self._client.request(method, path, **kwargs)
        if response.status_code >= 400:
            try:
                error = response.json()
                msg = error.get("error", response.text)
            except Exception:
                msg = response.text
            raise RuntimeError(f"Dune API error ({response.status_code}): {msg}")
        return response.json()

    def execute_query(self, query_id: int, params: dict[str, Any] | None = None) -> dict:
        """Execute a query and return execution ID.

        Args:
            query_id: The Dune query ID
            params: Optional query parameters

        Returns:
            Dict with execution_id and state
        """
        body = {}
        if params:
            body["query_parameters"] = params
        return self._request("POST", f"/query/{query_id}/execute", json=body)

    def get_execution_status(self, execution_id: str) -> dict:
        """Get the status of a query execution.

        Args:
            execution_id: The execution ID

        Returns:
            Dict with state, queue position, etc.
        """
        return self._request("GET", f"/execution/{execution_id}/status")

    def get_execution_results(self, execution_id: str) -> dict:
        """Get the results of a completed execution.

        Args:
            execution_id: The execution ID

        Returns:
            Dict with result rows and metadata
        """
        return self._request("GET", f"/execution/{execution_id}/results")

    def cancel_execution(self, execution_id: str) -> dict:
        """Cancel a running execution.

        Args:
            execution_id: The execution ID

        Returns:
            Cancellation confirmation
        """
        return self._request("POST", f"/execution/{execution_id}/cancel")

    def get_query(self, query_id: int) -> dict:
        """Get query metadata.

        Args:
            query_id: The Dune query ID

        Returns:
            Query metadata including name, description, parameters
        """
        return self._request("GET", f"/query/{query_id}")

    def raw_request(
        self,
        method: str,
        endpoint: str,
        json: dict | None = None,
        params: dict | None = None,
    ) -> dict:
        """Make a raw API call.

        Args:
            method: HTTP method
            endpoint: API endpoint path (relative to /api/v1, e.g. "/query/123")
            json: Optional JSON body
            params: Optional query parameters

        Returns:
            JSON response
        """
        if endpoint.startswith("/api/v1"):
            endpoint = endpoint[len("/api/v1"):]
        kwargs: dict = {}
        if json is not None:
            kwargs["json"] = json
        if params is not None:
            kwargs["params"] = params
        return self._request(method, endpoint, **kwargs)

    def close(self):
        """Close the underlying HTTP client."""
        self._client.close()


def _client() -> DuneClient:
    """Factory function for tool SDK."""
    from centaur_sdk import secret

    return DuneClient(api_key=secret("DUNE_API_KEY"))
