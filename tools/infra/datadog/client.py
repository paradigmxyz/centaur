"""Datadog HTTP API client for read-only observability workflows."""

import os
from datetime import UTC, datetime, timedelta
from typing import Any

import httpx

from centaur_sdk import secret

_DEFAULT_SITE = "https://api.datadoghq.com"


class DatadogClient:
    """Client for Datadog REST APIs.

    Uses ``DD_API_KEY`` and ``DD_APP_KEY``. ``DD_SITE`` may override the API site,
    for example ``https://api.datadoghq.eu`` or ``https://api.us5.datadoghq.com``.
    """

    def __init__(
        self,
        site: str | None = None,
        api_key: str | None = None,
        app_key: str | None = None,
        timeout: float = 30.0,
    ):
        self._site = site
        self._api_key = api_key
        self._app_key = app_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        site = (self._site or os.getenv("DD_SITE", _DEFAULT_SITE)).rstrip("/")  # noqa: TID251
        if not site.startswith(("http://", "https://")):
            site = f"https://{site}"
        return site

    def _auth_headers(self) -> dict[str, str]:
        api_key = self._api_key or secret("DD_API_KEY", "")
        app_key = self._app_key or secret("DD_APP_KEY", "")
        headers: dict[str, str] = {"Accept": "application/json"}
        if api_key:
            headers["DD-API-KEY"] = api_key
        if app_key:
            headers["DD-APPLICATION-KEY"] = app_key
        return headers

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(
                base_url=self.base_url,
                headers=self._auth_headers(),
                timeout=self.timeout,
                follow_redirects=True,
            )
        return self._client

    def _request(
        self,
        method: str,
        path: str,
        params: dict | None = None,
        json_data: dict | None = None,
    ) -> Any:
        clean = {k: v for k, v in (params or {}).items() if v is not None}
        resp = self.client.request(method, path, params=clean, json=json_data)
        if resp.status_code >= 400:
            raise RuntimeError(f"Datadog API error ({resp.status_code}): {resp.text}")
        if not resp.content:
            return {}
        return resp.json()

    def validate(self) -> dict:
        """Validate API/application keys with Datadog's auth endpoint."""
        return self._request("GET", "/api/v1/validate")

    def search_logs(
        self,
        query: str,
        start: str = "15m",
        end: str | None = None,
        limit: int = 50,
        sort: str = "-timestamp",
    ) -> dict:
        """Search logs using Datadog log search syntax.

        Args:
            query: Datadog logs query, e.g. ``service:api status:error``.
            start: Start time. Relative forms like ``15m``, ``2h``, ``7d`` are accepted.
            end: End time. Defaults to now.
            limit: Max logs to return, clamped to 1-1000.
            sort: ``timestamp`` or ``-timestamp``.
        """
        payload = {
            "filter": {
                "query": query,
                "from": _resolve_time(start).isoformat().replace("+00:00", "Z"),
                "to": _resolve_time(end).isoformat().replace("+00:00", "Z"),
            },
            "sort": sort,
            "page": {"limit": max(1, min(limit, 1000))},
        }
        return self._request("POST", "/api/v2/logs/events/search", json_data=payload)

    def query_metrics(
        self,
        query: str,
        start: str = "1h",
        end: str | None = None,
    ) -> dict:
        """Query Datadog metrics over a time window.

        Args:
            query: Datadog metric query, e.g. ``avg:system.cpu.user{*}``.
            start: Start time. Relative forms like ``1h`` or epoch seconds are accepted.
            end: End time. Defaults to now.
        """
        return self._request(
            "GET",
            "/api/v1/query",
            params={
                "query": query,
                "from": int(_resolve_time(start).timestamp()),
                "to": int(_resolve_time(end).timestamp()),
            },
        )

    def list_monitors(
        self,
        query: str | None = None,
        group_states: str | None = None,
        tags: str | None = None,
        limit: int = 100,
    ) -> list[dict]:
        """List monitors, optionally filtered by query, states, or tags."""
        return self._request(
            "GET",
            "/api/v1/monitor",
            params={
                "monitor_tags": tags,
                "group_states": group_states,
                "name": query,
                "page_size": max(1, min(limit, 1000)),
            },
        )

    def search_monitors(
        self,
        query: str = "",
        page: int = 0,
        per_page: int = 30,
    ) -> dict:
        """Search monitors with Datadog monitor search syntax."""
        return self._request(
            "GET",
            "/api/v1/monitor/search",
            params={"query": query, "page": page, "per_page": max(1, min(per_page, 100))},
        )

    def get_monitor(self, monitor_id: int) -> dict:
        """Get one monitor by numeric id."""
        return self._request("GET", f"/api/v1/monitor/{monitor_id}")

    def list_hosts(
        self,
        filter: str | None = None,
        sort_field: str = "last_reported_time",
        sort_dir: str = "desc",
        count: int = 100,
    ) -> dict:
        """List hosts visible in Datadog."""
        return self._request(
            "GET",
            "/api/v1/hosts",
            params={
                "filter": filter,
                "sort_field": sort_field,
                "sort_dir": sort_dir,
                "count": max(1, min(count, 1000)),
            },
        )

    def search_dashboards(self, query: str | None = None, limit: int = 100) -> dict:
        """Search dashboards."""
        return self._request(
            "GET",
            "/api/v1/dashboard",
            params={"filter[query]": query, "count": max(1, min(limit, 1000))},
        )

    def get_dashboard(self, dashboard_id: str) -> dict:
        """Get a dashboard by id."""
        return self._request("GET", f"/api/v1/dashboard/{dashboard_id}")

    def close(self):
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _resolve_time(value: str | None) -> datetime:
    if not value:
        return datetime.now(UTC)
    raw = value.strip()
    if raw.isdigit():
        timestamp = int(raw)
        if timestamp > 10_000_000_000:
            timestamp = timestamp // 1000
        return datetime.fromtimestamp(timestamp, UTC)
    unit = raw[-1]
    amount = raw[:-1]
    if amount.isdigit() and unit in {"s", "m", "h", "d", "w"}:
        seconds = int(amount) * {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}[unit]
        return datetime.now(UTC) - timedelta(seconds=seconds)
    normalized = raw.replace("Z", "+00:00")
    parsed = datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=UTC)
    return parsed.astimezone(UTC)


def _client() -> DatadogClient:
    return DatadogClient()
