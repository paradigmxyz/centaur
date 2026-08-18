"""Amplitude Dashboard REST + Taxonomy API client.

Mirrors the read/query surface of the Amplitude MCP using the Dashboard REST
API (event segmentation, funnels, retention, user activity, realtime) and the
Taxonomy API (tracking-plan event and property definitions). Authenticates with
HTTP Basic using the project's API key and secret key.

The Amplitude MCP's write surface — creating charts, dashboards, experiments,
and feature flags — requires OAuth and is not exposed by these API-key REST
endpoints, so it is intentionally out of scope here.
"""

import base64
import json
import os

import httpx

from centaur_sdk import secret

_US_HOST = "amplitude.com"
_EU_HOST = "analytics.eu.amplitude.com"


def _fmt_date(value: str) -> str:
    """Normalize a date to Amplitude's YYYYMMDD format.

    Accepts YYYY-MM-DD, YYYY/MM/DD, or YYYYMMDD.
    """
    return value.replace("-", "").replace("/", "").strip()


class AmplitudeClient:
    """Client for the Amplitude Dashboard REST and Taxonomy APIs.

    Requires a project API key and secret key (Settings > Projects > General).
    Set AMPLITUDE_REGION to "eu" for EU-residency projects.
    """

    def __init__(
        self,
        api_key: str | None = None,
        secret_key: str | None = None,
        region: str | None = None,
        timeout: float = 60.0,
    ):
        """Initialize the Amplitude client.

        Args:
            api_key: Project API key (or set AMPLITUDE_API_KEY)
            secret_key: Project secret key (or set AMPLITUDE_SECRET_KEY)
            region: Data residency region, "us" (default) or "eu"
            timeout: Request timeout in seconds
        """
        self._api_key = api_key
        self._secret_key = secret_key
        self._region = region
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def region(self) -> str:
        region = self._region or os.getenv("AMPLITUDE_REGION", "us")  # noqa: TID251
        return region.strip().lower()

    @property
    def host(self) -> str:
        return _EU_HOST if self.region == "eu" else _US_HOST

    @property
    def base_url(self) -> str:
        return f"https://{self.host}"

    def _credentials(self) -> tuple[str, str]:
        api_key = self._api_key or secret("AMPLITUDE_API_KEY", "")
        secret_key = self._secret_key or secret("AMPLITUDE_SECRET_KEY", "")
        if not api_key or not secret_key:
            raise RuntimeError(
                "AMPLITUDE_API_KEY and AMPLITUDE_SECRET_KEY must be set "
                "(Settings > Projects > General in Amplitude)."
            )
        return api_key, secret_key

    def _auth_header(self) -> str:
        api_key, secret_key = self._credentials()
        token = base64.b64encode(f"{api_key}:{secret_key}".encode()).decode()
        return f"Basic {token}"

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(base_url=self.base_url, timeout=self.timeout)
        return self._client

    def _request(
        self,
        method: str,
        endpoint: str,
        params: dict | list | None = None,
    ) -> dict:
        """Make an authenticated Dashboard REST / Taxonomy API request.

        Args:
            method: HTTP method (GET, POST, etc.)
            endpoint: API endpoint path
            params: Query parameters (dict, or list of tuples for repeated keys)

        Returns:
            JSON response data

        Raises:
            RuntimeError: If the request fails
        """
        headers = {"Authorization": self._auth_header()}
        try:
            response = self.client.request(method, endpoint, headers=headers, params=params)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"API error: {e.response.status_code} - {e.response.text}") from e
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}") from e

    def _event_spec(
        self,
        event_type: str,
        filters: list[dict] | None = None,
        group_by: list[dict] | None = None,
    ) -> str:
        """Build the compact JSON for a Dashboard REST `e` event parameter."""
        spec: dict = {"event_type": event_type}
        if filters:
            spec["filters"] = filters
        if group_by:
            spec["group_by"] = group_by
        return json.dumps(spec, separators=(",", ":"))

    def segmentation(
        self,
        event: str,
        start: str,
        end: str,
        metric: str = "totals",
        interval: int = 1,
        group_by: str | None = None,
        group_by_type: str = "event",
        segment: list[dict] | None = None,
        limit: int = 100,
    ) -> dict:
        """Event segmentation — counts/uniques of an event over time.

        Args:
            event: Event type (use "_all" for any active event)
            start: Start date (YYYYMMDD or YYYY-MM-DD)
            end: End date (YYYYMMDD or YYYY-MM-DD)
            metric: uniques, totals, pct_dau, average, histogram, sums, value_avg
            interval: 1 (daily), 7 (weekly), 30 (monthly)
            group_by: Property to break down by (optional, up to one here)
            group_by_type: "event" or "user" property for group_by
            segment: Segment filter definitions (JSON array)
            limit: Max breakdown groups returned

        Returns:
            Segmentation series data
        """
        gb = [{"type": group_by_type, "value": group_by}] if group_by else None
        params: list[tuple[str, str]] = [
            ("e", self._event_spec(event, group_by=gb)),
            ("start", _fmt_date(start)),
            ("end", _fmt_date(end)),
            ("m", metric),
            ("i", str(interval)),
            ("limit", str(limit)),
        ]
        if segment:
            params.append(("s", json.dumps(segment, separators=(",", ":"))))
        return self._request("GET", "/api/2/events/segmentation", params=params)

    def funnel(
        self,
        events: list[str],
        start: str,
        end: str,
        mode: str = "ordered",
        conversion_window_days: int | None = None,
    ) -> dict:
        """Funnel conversion across an ordered sequence of events.

        Args:
            events: Event types in step order (one `e` param per step)
            start: Start date (YYYYMMDD or YYYY-MM-DD)
            end: End date (YYYYMMDD or YYYY-MM-DD)
            mode: ordered, unordered, or sequential
            conversion_window_days: Conversion window in days (optional)

        Returns:
            Funnel conversion data
        """
        params: list[tuple[str, str]] = [("e", self._event_spec(e)) for e in events]
        params += [
            ("start", _fmt_date(start)),
            ("end", _fmt_date(end)),
            ("mode", mode),
        ]
        if conversion_window_days is not None:
            params.append(("cs", str(conversion_window_days * 86_400_000)))
        return self._request("GET", "/api/2/funnels", params=params)

    def retention(
        self,
        start_event: str,
        return_event: str,
        start: str,
        end: str,
        retention_mode: str = "n-day",
        interval: int = 1,
    ) -> dict:
        """Retention analysis between a start event and a return event.

        Args:
            start_event: Event that starts the retention measurement
            return_event: Event that counts as a return ("_all" for any)
            start: Start date (YYYYMMDD or YYYY-MM-DD)
            end: End date (YYYYMMDD or YYYY-MM-DD)
            retention_mode: n-day, rolling, or bracket
            interval: 1 (daily), 7 (weekly), 30 (monthly)

        Returns:
            Retention data
        """
        params: list[tuple[str, str]] = [
            ("se", self._event_spec(start_event)),
            ("re", self._event_spec(return_event)),
            ("start", _fmt_date(start)),
            ("end", _fmt_date(end)),
            ("rm", retention_mode),
            ("i", str(interval)),
        ]
        return self._request("GET", "/api/2/retention", params=params)

    def events_list(self) -> dict:
        """List every event type defined in the project."""
        return self._request("GET", "/api/2/events/list")

    def user_activity(
        self,
        user: str,
        limit: int = 100,
        offset: int = 0,
        direction: str = "latest",
    ) -> dict:
        """Fetch a single user's event stream by Amplitude ID.

        Args:
            user: Amplitude ID
            limit: Max events (up to 1000)
            offset: Pagination offset
            direction: "latest" or "earliest"

        Returns:
            User details and recent events
        """
        params = {"user": user, "limit": limit, "offset": offset, "direction": direction}
        return self._request("GET", "/api/2/useractivity", params=params)

    def user_search(self, user: str) -> dict:
        """Search for a user by Amplitude ID, Device ID, User ID, or prefix.

        Args:
            user: Identifier or prefix to match

        Returns:
            Matching users
        """
        return self._request("GET", "/api/2/usersearch", params={"user": user})

    def realtime(self, interval: int | None = None) -> dict:
        """Real-time active user counts.

        Args:
            interval: Interval in seconds (e.g. 300 for 5-minute buckets)

        Returns:
            Realtime active-user series
        """
        params = {"i": -interval * 1000} if interval else None
        return self._request("GET", "/api/2/realtime", params=params)

    def annotations(self) -> dict:
        """List chart annotations for the project."""
        return self._request("GET", "/api/2/annotations")

    def taxonomy_events(self) -> dict:
        """List event types in the tracking plan (Taxonomy API)."""
        return self._request("GET", "/api/2/taxonomy/event")

    def taxonomy_event_properties(self, event_type: str) -> dict:
        """List event properties defined for an event type (Taxonomy API).

        Args:
            event_type: Event type whose properties to list
        """
        return self._request(
            "GET", "/api/2/taxonomy/event-property", params={"event_type": event_type}
        )

    def taxonomy_user_properties(self) -> dict:
        """List user properties in the tracking plan (Taxonomy API)."""
        return self._request("GET", "/api/2/taxonomy/user-property")

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> AmplitudeClient:
    api_key = secret("AMPLITUDE_API_KEY", "")
    secret_key = secret("AMPLITUDE_SECRET_KEY", "")
    return AmplitudeClient(api_key=api_key, secret_key=secret_key)
