"""Client for Luma's public calendar and event API."""

from __future__ import annotations

import time
from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://public-api.luma.com"


class LumaClient:
    """Authenticated client for Luma calendars, events, guests, and related resources."""

    def __init__(
        self,
        api_key: str | None = None,
        *,
        timeout: float = 30.0,
        max_rate_limit_retries: int = 2,
    ) -> None:
        self._api_key_override = api_key
        self._timeout = timeout
        self._http_client: httpx.Client | None = None
        self.max_rate_limit_retries = max_rate_limit_retries

    def _http(self) -> httpx.Client:
        if self._http_client is not None:
            return self._http_client

        api_key = self._api_key_override or secret("LUMA_API_KEY", "")
        if not api_key:
            raise RuntimeError(
                "LUMA_API_KEY not set. Generate one at https://luma.com/calendar/manage/api-keys"
            )
        self._http_client = httpx.Client(
            base_url=BASE_URL,
            headers={"x-luma-api-key": api_key, "Accept": "application/json"},
            timeout=self._timeout,
        )
        return self._http_client

    @staticmethod
    def _endpoint(endpoint: str) -> str:
        """Normalize an API path without allowing requests outside Luma's API host."""
        path = endpoint.strip()
        if not path:
            raise ValueError("endpoint is required")
        if "://" in path or path.startswith("//"):
            raise ValueError("endpoint must be a Luma API path, not a URL")
        if not path.startswith("/"):
            path = f"/{path}"
        if not path.startswith(("/v1/", "/v2/")):
            raise ValueError("endpoint must start with /v1/ or /v2/")
        return path

    @staticmethod
    def _retry_delay(response: httpx.Response) -> float:
        try:
            delay = float(response.headers.get("Retry-After", "1"))
        except ValueError:
            delay = 1.0
        return min(max(delay, 0.0), 60.0)

    @staticmethod
    def _error_message(response: httpx.Response) -> str:
        try:
            payload = response.json()
        except ValueError:
            return response.text.strip() or response.reason_phrase

        if isinstance(payload, dict):
            for key in ("message", "error", "detail"):
                value = payload.get(key)
                if isinstance(value, str) and value:
                    return value
                if isinstance(value, dict):
                    nested = value.get("message")
                    if isinstance(nested, str) and nested:
                        return nested
        return response.text.strip() or response.reason_phrase

    def request(
        self,
        method: str,
        endpoint: str,
        *,
        params: dict[str, Any] | list[tuple[str, Any]] | None = None,
        json: dict[str, Any] | None = None,
        calendar_id: str | None = None,
    ) -> dict[str, Any] | list[Any]:
        """Call any Luma API endpoint.

        Use ``calendar_id`` with an organization API key to send the
        ``x-luma-calendar-id`` header. Array-valued query parameters are encoded
        as repeated parameters, as required by Luma.
        """
        headers = {"x-luma-calendar-id": calendar_id} if calendar_id else None
        for attempt in range(self.max_rate_limit_retries + 1):
            response = self._http().request(
                method.upper(),
                self._endpoint(endpoint),
                params=params,
                json=json,
                headers=headers,
            )
            if response.status_code != 429 or attempt == self.max_rate_limit_retries:
                break
            time.sleep(self._retry_delay(response))

        if response.status_code >= 400:
            raise RuntimeError(
                f"Luma API error ({response.status_code}): {self._error_message(response)}"
            )
        if response.status_code == 204 or not response.content:
            return {}
        data = response.json()
        if not isinstance(data, (dict, list)):
            raise RuntimeError("Luma API returned an unexpected JSON response")
        return data

    def get_self(self) -> dict[str, Any]:
        """Get the user associated with the API key."""
        return self.request("GET", "/v1/users/get-self")  # type: ignore[return-value]

    def get_calendar(self, *, calendar_id: str | None = None) -> dict[str, Any]:
        """Get the calendar associated with the key, or an organization calendar."""
        return self.request(  # type: ignore[return-value]
            "GET", "/v1/calendars/get", calendar_id=calendar_id
        )

    def list_events(
        self,
        *,
        before: str | None = None,
        after: str | None = None,
        cursor: str | None = None,
        limit: int | None = None,
        platforms: list[str] | None = None,
        access: list[str] | None = None,
        status: str | None = None,
        sort_direction: str | None = None,
        calendar_id: str | None = None,
    ) -> dict[str, Any]:
        """List calendar events, returning entries and cursor pagination metadata."""
        params = {
            "before": before,
            "after": after,
            "pagination_cursor": cursor,
            "pagination_limit": limit,
            "platforms": platforms,
            "access": access,
            "status": status,
            "sort_column": "start_at" if sort_direction else None,
            "sort_direction": sort_direction,
        }
        return self.request(  # type: ignore[return-value]
            "GET",
            "/v1/calendars/events/list",
            params={key: value for key, value in params.items() if value is not None},
            calendar_id=calendar_id,
        )

    def get_event(self, event_id: str) -> dict[str, Any]:
        """Get an event by its ``evt-`` ID."""
        return self.request(  # type: ignore[return-value]
            "GET", "/v1/events/get", params={"event_id": event_id}
        )

    def create_event(self, event: dict[str, Any], *, calendar_id: str | None = None) -> dict:
        """Create an event from a Luma EventCreateRequest object."""
        return self.request(  # type: ignore[return-value]
            "POST", "/v1/events/create", json=event, calendar_id=calendar_id
        )

    def update_event(self, event: dict[str, Any], *, calendar_id: str | None = None) -> dict:
        """Update an event; the object must include ``event_id``."""
        return self.request(  # type: ignore[return-value]
            "POST", "/v1/events/update", json=event, calendar_id=calendar_id
        )

    def list_guests(
        self,
        event_id: str,
        *,
        approval_status: str | None = None,
        cursor: str | None = None,
        limit: int | None = None,
        sort_column: str | None = None,
        sort_direction: str | None = None,
    ) -> dict[str, Any]:
        """List guests for an event, returning entries and pagination metadata."""
        params = {
            "event_id": event_id,
            "approval_status": approval_status,
            "pagination_cursor": cursor,
            "pagination_limit": limit,
            "sort_column": sort_column,
            "sort_direction": sort_direction,
        }
        return self.request(  # type: ignore[return-value]
            "GET",
            "/v1/events/guests/list",
            params={key: value for key, value in params.items() if value is not None},
        )

    def close(self) -> None:
        """Close the underlying HTTP connection pool."""
        if self._http_client is not None:
            self._http_client.close()
            self._http_client = None

    def __enter__(self) -> LumaClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def _client() -> LumaClient:
    return LumaClient()
