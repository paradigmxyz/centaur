"""Client for the FRED (Federal Reserve Economic Data) API.

API reference: https://fred.stlouisfed.org/docs/api/fred/
"""

from __future__ import annotations

from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://api.stlouisfed.org/fred"


class FredClient:
    """Read-only client for the FRED API."""

    def __init__(self, api_key: str | None = None, timeout: float = 30.0) -> None:
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _request(self, path: str, **params: Any) -> dict[str, Any]:
        query = {key: value for key, value in params.items() if value is not None}
        query["api_key"] = self._api_key or secret("FRED_API_KEY")
        query["file_type"] = "json"
        response = self.client.get(f"{BASE_URL}{path}", params=query)
        response.raise_for_status()
        result = response.json()
        if not isinstance(result, dict):
            raise RuntimeError("FRED returned an unexpected response.")
        return result

    # Series

    def search_series(
        self,
        text: str,
        search_type: str = "full_text",
        limit: int = 20,
        offset: int = 0,
        order_by: str | None = None,
        sort_order: str | None = None,
        tag_names: str | None = None,
    ) -> dict[str, Any]:
        """Search economic data series by keywords (search_type: full_text or series_id)."""
        return self._request(
            "/series/search",
            search_text=text,
            search_type=search_type,
            limit=limit,
            offset=offset,
            order_by=order_by,
            sort_order=sort_order,
            tag_names=tag_names,
        )

    def get_series(self, series_id: str) -> dict[str, Any]:
        """Get metadata for one series (title, units, frequency, last updated)."""
        return self._request("/series", series_id=series_id)

    def get_observations(
        self,
        series_id: str,
        start: str | None = None,
        end: str | None = None,
        units: str | None = None,
        frequency: str | None = None,
        aggregation_method: str | None = None,
        limit: int | None = None,
        sort_order: str = "asc",
    ) -> dict[str, Any]:
        """Get data values for a series.

        Dates are YYYY-MM-DD. ``units`` transforms values (lin, chg, ch1, pch, pc1,
        pca, cch, cca, log). ``frequency`` aggregates to a lower frequency (d, w, m,
        q, sa, a, ...) using ``aggregation_method`` (avg, sum, eop).
        """
        return self._request(
            "/series/observations",
            series_id=series_id,
            observation_start=start,
            observation_end=end,
            units=units,
            frequency=frequency,
            aggregation_method=aggregation_method,
            limit=limit,
            sort_order=sort_order,
        )

    def get_series_categories(self, series_id: str) -> dict[str, Any]:
        """Get the categories a series belongs to."""
        return self._request("/series/categories", series_id=series_id)

    def get_series_release(self, series_id: str) -> dict[str, Any]:
        """Get the release a series belongs to."""
        return self._request("/series/release", series_id=series_id)

    def get_series_tags(self, series_id: str) -> dict[str, Any]:
        """Get the tags attached to a series."""
        return self._request("/series/tags", series_id=series_id)

    def get_series_updates(self, limit: int = 20, offset: int = 0) -> dict[str, Any]:
        """List recently updated series."""
        return self._request("/series/updates", limit=limit, offset=offset)

    # Categories

    def get_category(self, category_id: int = 0) -> dict[str, Any]:
        """Get a category (0 is the root)."""
        return self._request("/category", category_id=category_id)

    def get_category_children(self, category_id: int = 0) -> dict[str, Any]:
        """List child categories of a category."""
        return self._request("/category/children", category_id=category_id)

    def get_category_series(
        self, category_id: int, limit: int = 20, offset: int = 0
    ) -> dict[str, Any]:
        """List series in a category."""
        return self._request(
            "/category/series", category_id=category_id, limit=limit, offset=offset
        )

    # Releases

    def list_releases(self, limit: int = 20, offset: int = 0) -> dict[str, Any]:
        """List all releases of economic data."""
        return self._request("/releases", limit=limit, offset=offset)

    def get_release(self, release_id: int) -> dict[str, Any]:
        """Get one release."""
        return self._request("/release", release_id=release_id)

    def get_release_series(
        self, release_id: int, limit: int = 20, offset: int = 0
    ) -> dict[str, Any]:
        """List series in a release."""
        return self._request("/release/series", release_id=release_id, limit=limit, offset=offset)

    def get_release_dates(
        self,
        release_id: int | None = None,
        start: str | None = None,
        end: str | None = None,
        limit: int = 20,
        include_future: bool = False,
        sort_order: str | None = None,
    ) -> dict[str, Any]:
        """Get release dates for one release, or for all releases when no ID is given."""
        params: dict[str, Any] = {
            "realtime_start": start,
            "realtime_end": end,
            "limit": limit,
            "sort_order": sort_order,
            "include_release_dates_with_no_data": "true" if include_future else None,
        }
        if release_id is None:
            return self._request("/releases/dates", **params)
        return self._request("/release/dates", release_id=release_id, **params)

    # Sources and tags

    def list_sources(self, limit: int = 20, offset: int = 0) -> dict[str, Any]:
        """List data sources."""
        return self._request("/sources", limit=limit, offset=offset)

    def list_tags(
        self, search_text: str | None = None, limit: int = 20, offset: int = 0
    ) -> dict[str, Any]:
        """List or search tags."""
        return self._request("/tags", search_text=search_text, limit=limit, offset=offset)

    def get_tag_series(self, tag_names: str, limit: int = 20, offset: int = 0) -> dict[str, Any]:
        """List series matching all of the semicolon-separated tag names."""
        return self._request("/tags/series", tag_names=tag_names, limit=limit, offset=offset)

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> FredClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _client() -> FredClient:
    return FredClient()
