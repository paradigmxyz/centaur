"""SimilarWeb API client."""

from datetime import date
from typing import Any
from typing import Literal

import httpx

from centaur_sdk import secret


def _clean_secret(value: str | None) -> str | None:
    """Clean a secret value that may be a multi-line 1Password blob."""
    if not value:
        return value
    value = value.strip()
    if "\n" not in value:
        return value or None
    for line in value.splitlines():
        line = line.strip()
        if not line or line.startswith("===") or line.startswith("#"):
            continue
        return line
    return None


def _add_months(d: date, months: int) -> date:
    """Return the first day of the month offset by ``months``."""
    month_index = d.year * 12 + d.month - 1 + months
    return date(month_index // 12, month_index % 12 + 1, 1)


def default_app_download_window(today: date | None = None) -> tuple[date, date]:
    """Return six monthly periods ending two months before today."""
    today = today or date.today()
    end = _add_months(today.replace(day=1), -2)
    start = _add_months(end, -5)
    return start, end


class SimilarWebClient:

    """Client for SimilarWeb API.

    API docs: https://developers.similarweb.com/reference
    Base URL: https://api.similarweb.com
    Auth: api_key query parameter
    """

    def __init__(self, api_key: str | None = None, timeout: float = 30.0):
        self._api_key = _clean_secret(api_key)
        self.base_url = "https://api.similarweb.com"
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _get_api_key(self) -> str:
        """Get API key from instance or env var."""
        if self._api_key:
            return self._api_key
        key = _clean_secret(secret("SIMILARWEB_API_KEY", ""))
        if not key:
            raise RuntimeError(
                "SIMILARWEB_API_KEY not set. Get your API key from SimilarWeb account settings."
            )
        return key

    @staticmethod
    def _error_message(response: httpx.Response) -> str:
        """Extract a readable SimilarWeb error message."""
        try:
            body: Any = response.json()
        except ValueError:
            return response.text

        if isinstance(body, dict):
            for key in ("error_message", "message", "error", "detail"):
                value = body.get(key)
                if value:
                    return str(value)
            return response.text

        return response.text

    def _request(
        self,
        endpoint: str,
        method: str = "GET",
        params: dict | None = None,
        json_data: dict | None = None,
    ) -> dict | list:
        """Make an authenticated API request."""
        url = f"{self.base_url}{endpoint}"
        if params is None:
            params = {}
        params["api_key"] = self._get_api_key()

        try:
            if method == "GET":
                response = self.client.get(url, params=params)
            elif method == "POST":
                response = self.client.post(url, params=params, json=json_data)
            else:
                raise ValueError(f"Unsupported method: {method}")

            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            message = self._error_message(e.response)
            raise RuntimeError(f"API error: {e.response.status_code} - {message}")
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}")

    @staticmethod
    def _format_date(d: date | str) -> str:
        """Format date as YYYY-MM."""
        if isinstance(d, str):
            return d[:7] if len(d) > 7 else d
        return d.strftime("%Y-%m")

    @staticmethod
    def _format_date_full(d: date | str) -> str:
        """Format date as YYYY-MM-DD."""
        if isinstance(d, str):
            return d
        return d.strftime("%Y-%m-%d")

    def get_visits(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        granularity: Literal["daily", "weekly", "monthly"] = "monthly",
        main_domain_only: bool = True,
    ) -> dict:
        """Get total visits (desktop + mobile).

        Args:
            domain: Website domain (e.g., 'google.com')
            start_date: Start date
            end_date: End date
            country: Country code or 'world' for global
            granularity: Data granularity
            main_domain_only: Exclude subdomains
        """
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "granularity": granularity,
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(
            f"/v1/website/{domain}/total-traffic-and-engagement/visits", params=params
        )

    def get_traffic_overview(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        granularity: Literal["daily", "weekly", "monthly"] = "monthly",
        main_domain_only: bool = True,
    ) -> dict:
        """Get comprehensive traffic metrics (visits, page views, bounce rate, etc.).

        Returns visits, pages per visit, avg visit duration, bounce rate.
        """
        visits = self.get_visits(
            domain, start_date, end_date, country, granularity, main_domain_only
        )
        pages = self.get_pages_per_visit(
            domain, start_date, end_date, country, granularity, main_domain_only
        )
        duration = self.get_avg_visit_duration(
            domain, start_date, end_date, country, granularity, main_domain_only
        )
        bounce = self.get_bounce_rate(
            domain, start_date, end_date, country, granularity, main_domain_only
        )

        return {
            "domain": domain,
            "visits": visits,
            "pages_per_visit": pages,
            "avg_visit_duration": duration,
            "bounce_rate": bounce,
        }

    def get_pages_per_visit(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        granularity: Literal["daily", "weekly", "monthly"] = "monthly",
        main_domain_only: bool = True,
    ) -> dict:
        """Get pages per visit metric."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "granularity": granularity,
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(
            f"/v1/website/{domain}/total-traffic-and-engagement/pages-per-visit", params=params
        )

    def get_avg_visit_duration(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        granularity: Literal["daily", "weekly", "monthly"] = "monthly",
        main_domain_only: bool = True,
    ) -> dict:
        """Get average visit duration in seconds."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "granularity": granularity,
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(
            f"/v1/website/{domain}/total-traffic-and-engagement/average-visit-duration",
            params=params,
        )

    def get_bounce_rate(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        granularity: Literal["daily", "weekly", "monthly"] = "monthly",
        main_domain_only: bool = True,
    ) -> dict:
        """Get bounce rate metric."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "granularity": granularity,
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(
            f"/v1/website/{domain}/total-traffic-and-engagement/bounce-rate", params=params
        )

    def get_global_rank(self, domain: str) -> dict:
        """Get global SimilarWeb rank for a domain."""
        return self._request(f"/v1/website/{domain}/global-rank/global-rank")

    def get_country_rank(self, domain: str, country: str = "us") -> dict:
        """Get country-specific rank for a domain."""
        params = {"country": country}
        return self._request(f"/v1/website/{domain}/country-rank/country-rank", params=params)

    def get_industry_rank(self, domain: str, country: str = "world") -> dict:
        """Get industry/category rank for a domain."""
        params = {"country": country}
        return self._request(f"/v1/website/{domain}/category-rank/category-rank", params=params)

    def get_geography(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        main_domain_only: bool = True,
    ) -> dict:
        """Get traffic geography distribution by country."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(f"/v1/website/{domain}/geo/traffic-by-country", params=params)

    def get_traffic_sources(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        main_domain_only: bool = True,
    ) -> dict:
        """Get traffic sources breakdown by marketing channel."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(f"/v1/website/{domain}/traffic-sources/overview", params=params)

    def get_referrals(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        main_domain_only: bool = True,
    ) -> dict:
        """Get referring websites."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "main_domain_only": str(main_domain_only).lower(),
        }
        return self._request(f"/v1/website/{domain}/traffic-sources/referrals", params=params)

    def get_similar_sites(self, domain: str) -> dict:
        """Get similar/competitor websites."""
        return self._request(f"/v1/website/{domain}/similar-sites/similarsites")

    def get_website_description(self, domain: str) -> dict:
        """Get website description/metadata."""
        return self._request(f"/v1/website/{domain}/general-data/description")

    def get_top_sites(
        self,
        category: str | None = None,
        country: str = "world",
    ) -> dict:
        """Get top ranked websites by category.

        Args:
            category: Category path (e.g., 'Finance/Investing') or None for overall
            country: Country code or 'world'
        """
        params = {"country": country}
        if category:
            params["category"] = category
        return self._request("/v1/TopSites/categories", params=params)

    def get_keywords(
        self,
        domain: str,
        start_date: date | str,
        end_date: date | str,
        country: str = "world",
        limit: int = 100,
    ) -> dict:
        """Get organic and paid keywords for a website."""
        params = {
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
            "country": country,
            "limit": limit,
        }
        return self._request(f"/v1/website/{domain}/search-keywords/keywords", params=params)

    def get_app_details(self, app_id: str, store: Literal["google", "apple"] = "google") -> dict:
        """Get mobile app details.

        Args:
            app_id: App ID (package name for Android, numeric ID for iOS)
            store: 'google' or 'apple'
        """
        return self._request(f"/v1/app/{store}/{app_id}/details")

    def get_app_downloads(
        self,
        app_id: str,
        store: Literal["google", "apple"] = "google",
        start_date: date | str | None = None,
        end_date: date | str | None = None,
        country: str = "world",
        granularity: Literal["daily", "weekly", "monthly"] = "monthly",
    ) -> dict:
        """Get app download estimates."""
        if start_date is None or end_date is None:
            default_start, default_end = default_app_download_window()
            start_date = start_date or default_start
            end_date = end_date or default_end

        params = {"country": country, "granularity": granularity}
        params["start_date"] = self._format_date(start_date)
        params["end_date"] = self._format_date(end_date)
        return self._request(f"/v5/apps/{store}/downloads", params={**params, "app_id": app_id})

    def get_app_rank(
        self,
        app_id: str,
        store: Literal["google", "apple"] = "google",
        country: str = "us",
    ) -> dict:
        """Get app store ranking."""
        params = {"country": country}
        return self._request(f"/v1/app/{store}/{app_id}/rank", params=params)

    def search_apps(
        self,
        query: str,
        store: Literal["google", "apple"] = "google",
    ) -> dict:
        """Search for mobile apps."""
        params = {"term": query}
        return self._request(f"/v1/app/{store}/search", params=params)

    def get_categories(self) -> dict:
        """Get list of available industry categories."""
        return self._request("/v1/TopSites/categories")

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()



def _client() -> SimilarWebClient:
    api_key = secret("SIMILARWEB_API_KEY", "")
    return SimilarWebClient(api_key=api_key)
