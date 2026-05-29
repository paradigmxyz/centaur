"""SensorTower API client."""

from datetime import date

import httpx

from centaur_sdk import secret


class SensorTowerClient:
    """Client for SensorTower API.

    API docs: https://app.sensortower.com/api/docs/app_analysis (requires login)
    Base URL: https://api.sensortower.com
    """

    def __init__(self, auth_token: str | None = None, timeout: float = 30.0):
        self._auth_token = auth_token
        self.base_url = "https://api.sensortower.com"
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _get_auth_token(self) -> str:
        """Get auth token from instance or env var."""
        if self._auth_token:
            return self._auth_token
        token = secret("SENSOR_TOWER_AUTH_TOKEN", "") or secret("SENSORTOWER_AUTH_TOKEN", "")
        if not token:
            raise RuntimeError(
                "SENSOR_TOWER_AUTH_TOKEN not set. "
                "Get your token from https://sensortower.com/users/edit"
            )
        return token

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
        params["auth_token"] = self._get_auth_token()

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
            raise RuntimeError(f"API error: {e.response.status_code} - {e.response.text}")
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}")

    @staticmethod
    def _format_date(d: date | str) -> str:
        """Format date as YYYY-MM-DD."""
        if isinstance(d, str):
            return d
        return d.strftime("%Y-%m-%d")

    @staticmethod
    def _os_to_platform(platform: str) -> str:
        """Convert platform name to API format."""
        platform = platform.lower()
        if platform in ("ios", "itunes", "apple"):
            return "ios"
        elif platform in ("android", "google", "play"):
            return "android"
        return platform

    def get_sales_estimates(
        self,
        app_ids: list[str],
        platform: str,
        start_date: date | str,
        end_date: date | str,
        countries: list[str] | None = None,
        date_granularity: str = "daily",
    ) -> dict:
        """Get download and revenue estimates for apps.

        Args:
            app_ids: List of app IDs (iOS numeric ID or Android package name)
            platform: 'ios' or 'android'
            start_date: Start date for data
            end_date: End date for data
            countries: List of country codes (e.g., ['US', 'GB']). None = worldwide
            date_granularity: 'daily', 'weekly', or 'monthly'
        """
        os_type = self._os_to_platform(platform)

        params = {
            "date_granularity": date_granularity,
            "start_date": self._format_date(start_date),
            "end_date": self._format_date(end_date),
        }

        if os_type == "ios":
            params["app_ids"] = ",".join(app_ids)
            endpoint = "/v1/ios/sales_report_estimates"
        else:
            params["app_ids"] = ",".join(app_ids)
            endpoint = "/v1/android/sales_report_estimates"

        if countries:
            params["countries"] = ",".join(countries)

        return self._request(endpoint, params=params)

    def get_top_charts(
        self,
        platform: str,
        category: str | None = None,
        country: str = "US",
        chart_type: str = "free",
        limit: int = 100,
        date: date | str | None = None,
    ) -> list:
        """Get top charts ranking.

        Args:
            platform: 'ios' or 'android'
            category: Category ID (e.g., '6014' for Games, '36' for overall on iOS). Required.
            country: Country code (e.g., 'US')
            chart_type: 'free', 'paid', or 'grossing'
            limit: Number of results (max 400)
            date: Date for rankings (YYYY-MM-DD). Required.
        """
        os_type = self._os_to_platform(platform)

        # Map simple chart_type to API-expected values
        chart_type_map = {
            "free": "topfreeapplications",
            "paid": "toppaidapplications",
            "grossing": "topgrossingapplications",
        }
        api_chart_type = chart_type_map.get(chart_type, chart_type)

        params = {
            "country": country,
            "chart_type": api_chart_type,
            "limit": min(limit, 400),
        }

        if category:
            params["category"] = category
        else:
            params["category"] = "0"  # Overall apps

        if date:
            params["date"] = self._format_date(date)
        else:
            from datetime import date as date_cls

            params["date"] = date_cls.today().isoformat()

        endpoint = f"/v1/{os_type}/ranking"
        return self._request(endpoint, params=params)

    def get_publisher(self, publisher_id: str, platform: str = "ios") -> dict:
        """Get publisher information.

        Args:
            publisher_id: Unified publisher ID (24-char hex)
            platform: 'ios' or 'android'
        """
        endpoint = "/v1/unified/publishers/apps"
        return self._request(endpoint, params={"unified_id": publisher_id})

    def get_publisher_apps(
        self,
        publisher_id: str,
        platform: str = "ios",
    ) -> list:
        """Get apps by publisher.

        Args:
            publisher_id: Unified publisher ID (24-char hex)
            platform: 'ios' or 'android'
        """
        endpoint = "/v1/unified/publishers/apps"
        return self._request(endpoint, params={"unified_id": publisher_id})

    def search_apps(
        self,
        query: str,
        platform: str = "ios",
        limit: int = 50,
    ) -> list:
        """Search for apps by name or keywords.

        Args:
            query: Search query
            platform: 'ios' or 'android'
            limit: Max results
        """
        os_type = self._os_to_platform(platform)
        params = {
            "term": query,
            "limit": min(limit, 100),
        }
        endpoint = f"/v1/{os_type}/search_entities"
        params["entity_type"] = "app"
        return self._request(endpoint, params=params)

    def get_app_info(self, app_id: str, platform: str = "ios") -> dict:
        """Get app metadata.

        Args:
            app_id: App ID (iOS numeric ID or Android package name)
            platform: 'ios' or 'android'
        """
        os_type = self._os_to_platform(platform)
        endpoint = f"/v1/{os_type}/apps"
        result = self._request(endpoint, params={"app_ids": app_id})
        if isinstance(result, dict) and "apps" in result and result["apps"]:
            return result["apps"][0]
        return result

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> SensorTowerClient:
    return SensorTowerClient()
