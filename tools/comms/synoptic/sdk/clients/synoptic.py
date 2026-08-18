"""Synoptic Twitter API client with retry logic and connection reuse."""
# ruff: noqa

from __future__ import annotations

import asyncio
import copy
import logging
import time
from dataclasses import dataclass, replace
from typing import Any
from urllib.parse import urlencode

import httpx
from tenacity import (
    retry,
    retry_if_exception_type,
    stop_after_attempt,
    wait_exponential,
)

from ..config import settings
from ..exceptions import RetryableHTTPError

logger = logging.getLogger(__name__)

FOLLOWERS_CACHE_TTL_SECONDS = 30.0
FOLLOWING_CACHE_TTL_SECONDS = 30.0
PROFILES_CACHE_TTL_SECONDS = 30.0 * 60.0
TWEETS_SEARCH_CACHE_TTL_SECONDS = 60.0  # 1 minute - tweets change frequently
TWEETS_LOOKUP_CACHE_TTL_SECONDS = 30.0 * 60.0  # 30 minutes - tweet content is stable
USER_TIMELINE_CACHE_TTL_SECONDS = 60.0  # 1 minute - timelines update frequently
BUDGET_EXCEEDED_ERROR = "API units budget exceeded"


@dataclass
class RetryConfig:
    """Configuration for retry behavior."""

    max_attempts: int = 3
    wait_min: float = 1.0
    wait_max: float = 30.0
    multiplier: float = 2.0


@dataclass
class ApiUsage:
    """Track per-run API usage."""

    consumed_units: int = 0
    returned_items: int = 0
    remaining_units: int | None = None
    price_per_item: float | None = None
    requests: int = 0


@dataclass
class _CacheEntry:
    expires_at: float
    value: dict


class _TTLCache:
    """In-memory TTL cache for API responses."""

    def __init__(self) -> None:
        self._entries: dict[str, _CacheEntry] = {}

    def get(self, key: str) -> dict | None:
        entry = self._entries.get(key)
        if entry is None:
            return None
        if entry.expires_at <= time.monotonic():
            self._entries.pop(key, None)
            return None
        return entry.value

    def set(self, key: str, value: dict, ttl_seconds: float) -> None:
        self._entries[key] = _CacheEntry(
            expires_at=time.monotonic() + ttl_seconds,
            value=value,
        )


def is_retryable_error(exc: Exception) -> bool:
    """Check if an exception is retryable."""
    if isinstance(exc, httpx.HTTPStatusError):
        # Retry on 5xx and 429 (rate limit)
        return exc.response.status_code >= 500 or exc.response.status_code == 429
    # Retry on connection/timeout errors
    return isinstance(exc, (httpx.TimeoutException, httpx.ConnectError))


class UnitsLimitExceededError(Exception):
    """Raised when a run exceeds the configured units budget."""


class SynopticClient:
    """Client for Synoptic Twitter API with retry logic and connection reuse.

    This client can be used standalone or as an async context manager.

    Examples:
        # Standalone usage (creates new client per request)
        client = SynopticClient(api_key="your-key")
        user = await client.get_user_by_screen_name("elonmusk")

        # Context manager usage (reuses connection)
        async with SynopticClient(api_key="your-key") as client:
            user = await client.get_user_by_screen_name("elonmusk")
            followers, cursor, meta = await client.get_followers("elonmusk", ids_only=True)

        # Injected httpx client (for connection pooling)
        async with httpx.AsyncClient() as http_client:
            client = SynopticClient(client=http_client)
            user = await client.get_user_by_screen_name("elonmusk")
    """

    def __init__(
        self,
        api_key: str | None = None,
        base_url: str | None = None,
        retry_config: RetryConfig | None = None,
        client: httpx.AsyncClient | None = None,
        enable_cache: bool = False,
        max_units: int | None = None,
    ):
        """
        Initialize the client.

        Args:
            api_key: Synoptic API key. Defaults to SYNOPTIC_API_KEY env var.
            base_url: API base URL. Defaults to SYNOPTIC_BASE_URL env var.
            retry_config: Custom retry configuration. Defaults to settings.
            client: Optional httpx.AsyncClient for connection reuse.
                    If not provided, creates a new client per request.
            enable_cache: Enable in-memory response caching.
            max_units: Optional limit for total consumed units in a run.
        """
        self.base_url = base_url or settings.SYNOPTIC_BASE_URL
        self.headers = {
            "x-api-key": api_key or settings.SYNOPTIC_API_KEY,
        }
        self._retry_config = retry_config or RetryConfig(
            max_attempts=settings.RETRY_MAX_ATTEMPTS,
            wait_min=settings.RETRY_WAIT_MIN,
            wait_max=settings.RETRY_WAIT_MAX,
            multiplier=settings.RETRY_MULTIPLIER,
        )
        self._client = client
        self._owns_client = client is None
        self._cache_enabled = enable_cache
        self._cache = _TTLCache() if enable_cache else None
        self._max_units = max_units
        self._budget_exhausted = False
        self._usage = ApiUsage()
        self._usage_lock = asyncio.Lock()

    async def __aenter__(self) -> "SynopticClient":
        """Context manager entry - create client if needed."""
        if self._client is None:
            self._client = httpx.AsyncClient(timeout=30.0)
            self._owns_client = True
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
        """Context manager exit - close client if we own it."""
        if self._owns_client and self._client is not None:
            await self._client.aclose()
            self._client = None

    def _get_retry_decorator(self):
        """Get the retry decorator with current settings."""
        return retry(
            retry=retry_if_exception_type(RetryableHTTPError),
            stop=stop_after_attempt(self._retry_config.max_attempts),
            wait=wait_exponential(
                multiplier=self._retry_config.multiplier,
                min=self._retry_config.wait_min,
                max=self._retry_config.wait_max,
            ),
            before_sleep=lambda retry_state: logger.warning(
                f"Retrying request (attempt {retry_state.attempt_number}): "
                f"{retry_state.outcome.exception()}"
            ),
        )

    def _cache_ttl(self, endpoint: str) -> float | None:
        if endpoint == "/twttr-api/users/followers":
            return FOLLOWERS_CACHE_TTL_SECONDS
        if endpoint == "/twttr-api/users/followings":
            return FOLLOWING_CACHE_TTL_SECONDS
        if endpoint == "/twttr-api/users/lookup":
            return PROFILES_CACHE_TTL_SECONDS
        if endpoint == "/twttr-api/tweets/search":
            return TWEETS_SEARCH_CACHE_TTL_SECONDS
        if endpoint == "/twttr-api/tweets/lookup":
            return TWEETS_LOOKUP_CACHE_TTL_SECONDS
        if endpoint.startswith("/twttr-api/users/") and endpoint.endswith("/timeline"):
            return USER_TIMELINE_CACHE_TTL_SECONDS
        return None

    def _cache_key(self, endpoint: str, params: dict | None) -> str:
        if not params:
            return endpoint
        items: list[tuple[str, str]] = []
        for key, value in params.items():
            if value is None:
                continue
            if isinstance(value, (list, tuple)):
                value_str = ",".join(str(item) for item in value)
            else:
                value_str = str(value)
            items.append((str(key), value_str))
        query = urlencode(sorted(items))
        return f"{endpoint}?{query}" if query else endpoint

    async def _ensure_budget(self) -> None:
        if self._max_units is None:
            return
        if self._budget_exhausted:
            raise UnitsLimitExceededError(BUDGET_EXCEEDED_ERROR)
        async with self._usage_lock:
            if self._usage.consumed_units >= self._max_units:
                self._budget_exhausted = True
                raise UnitsLimitExceededError(BUDGET_EXCEEDED_ERROR)

    @staticmethod
    def _coerce_int(value: Any) -> int | None:
        if value is None:
            return None
        try:
            return int(value)
        except (TypeError, ValueError):
            return None

    @staticmethod
    def _coerce_float(value: Any) -> float | None:
        if value is None:
            return None
        try:
            return float(value)
        except (TypeError, ValueError):
            return None

    async def _record_usage(self, data: Any) -> None:
        async with self._usage_lock:
            self._usage.requests += 1
            if not isinstance(data, dict):
                return
            consumed_units = self._coerce_int(data.get("consumed_units"))
            returned_items = self._coerce_int(data.get("returned_items"))
            remaining_units = self._coerce_int(data.get("remaining_units"))
            price_per_item = self._coerce_float(data.get("price_per_item"))
            if consumed_units is not None:
                self._usage.consumed_units += consumed_units
            if returned_items is not None:
                self._usage.returned_items += returned_items
            if remaining_units is not None:
                self._usage.remaining_units = remaining_units
            if price_per_item is not None:
                self._usage.price_per_item = price_per_item
            if (
                self._max_units is not None
                and self._usage.consumed_units >= self._max_units
            ):
                self._budget_exhausted = True

    def get_usage(self) -> ApiUsage:
        return replace(self._usage)

    @property
    def budget_exhausted(self) -> bool:
        return self._budget_exhausted

    async def get(
        self, endpoint: str, params: dict | None = None, timeout: float = 30.0
    ) -> dict:
        """Make a GET request with retry logic."""

        cache_key = None
        cache_ttl = None
        if self._cache_enabled:
            cache_ttl = self._cache_ttl(endpoint)
            if cache_ttl is not None and self._cache is not None:
                cache_key = self._cache_key(endpoint, params)
                cached = self._cache.get(cache_key)
                if cached is not None:
                    return copy.deepcopy(cached)

        @self._get_retry_decorator()
        async def _do_request() -> dict:
            await self._ensure_budget()
            client = self._client or httpx.AsyncClient(timeout=timeout)
            try:
                response = await client.get(
                    f"{self.base_url}{endpoint}",
                    headers=self.headers,
                    params=params,
                    timeout=timeout,
                )
                response.raise_for_status()
                data = response.json()
                await self._record_usage(data)
                if (
                    cache_ttl is not None
                    and cache_key is not None
                    and self._cache is not None
                ):
                    self._cache.set(cache_key, copy.deepcopy(data), cache_ttl)
                return data
            except Exception as e:
                if is_retryable_error(e):
                    raise RetryableHTTPError(e) from e
                raise
            finally:
                if self._client is None:
                    await client.aclose()

        return await _do_request()

    async def get_user_details(self, user_ids: list[str]) -> list[dict]:
        """Fetch user details for a list of user IDs."""
        if not user_ids:
            return []

        users = []
        # Batch lookup - max 50 IDs per request, process all batches
        for i in range(0, len(user_ids), 50):
            batch = user_ids[i : i + 50]
            params = {"user_ids": ",".join(batch)}
            data = await self.get("/twttr-api/users/lookup", params)

            for user in data.get("data", []):
                users.append(
                    {
                        "id": user.get("user_id"),
                        "username": user.get("screen_name"),
                        "name": user.get("name"),
                        "description": user.get("description"),
                        "followers_count": user.get("followers_count"),
                        "following_count": user.get("following_count"),
                    }
                )
        return users

    async def get_user_by_screen_name(self, screen_name: str) -> dict | None:
        """Fetch user details by screen name."""
        params = {"screen_name": screen_name}
        data = await self.get("/twttr-api/users/lookup", params)
        users = data.get("data", [])
        if users and len(users) > 0:
            return users[0]
        return None

    async def get_followers(
        self,
        username: str,
        cursor: str | None = None,
        ids_only: bool = False,
        max_results: int = 1000,
    ) -> tuple[list[dict] | list[str], str | None, dict | None]:
        """
        Fetch followers for a username. Returns (followers, next_cursor, metadata).

        Args:
            username: Twitter screen name
            cursor: Pagination cursor
            ids_only: If True, returns list of user ID strings instead of full user dicts.
            max_results: Number of results per page (1-1000)

        Returns:
            Tuple of (followers, next_cursor, metadata)
            - metadata includes consumed_units, remaining_units, returned_items,
              and price_per_item if available
        """
        params = {"screen_name": username, "max_results": max_results}
        if cursor:
            params["cursor"] = cursor

        data = await self.get("/twttr-api/users/followers", params)

        follower_data = data.get("data", {})
        follower_ids = follower_data.get("followers", [])
        next_cursor = follower_data.get("next_cursor")

        # Extract metadata
        metadata = {
            "consumed_units": data.get("consumed_units"),
            "remaining_units": data.get("remaining_units"),
            "returned_items": data.get("returned_items"),
            "price_per_item": data.get("price_per_item"),
        }

        # Convert next_cursor to string if it's a number (0 means no more pages)
        if next_cursor is not None and next_cursor != 0:
            next_cursor = str(next_cursor)
        else:
            next_cursor = None

        if ids_only:
            return [str(uid) for uid in follower_ids], next_cursor, metadata

        # Lookup full user details for the follower IDs
        if follower_ids:
            followers = await self.get_user_details(follower_ids)
        else:
            followers = []

        return followers, next_cursor, metadata

    async def get_following(
        self,
        username: str,
        cursor: str | None = None,
        ids_only: bool = False,
        max_results: int = 1000,
    ) -> tuple[list[dict] | list[str], str | None, dict | None]:
        """
        Fetch following for a username. Returns (following, next_cursor, metadata).

        Args:
            username: Twitter screen name
            cursor: Pagination cursor
            ids_only: If True, returns list of user ID strings instead of full user dicts.
            max_results: Number of results per page (1-1000)

        Returns:
            Tuple of (following, next_cursor, metadata)
            - metadata includes consumed_units, remaining_units, returned_items,
              and price_per_item if available
        """
        params = {"screen_name": username, "max_results": max_results}
        if cursor:
            params["cursor"] = cursor

        data = await self.get("/twttr-api/users/followings", params)

        following_data = data.get("data", {})
        following_ids = following_data.get("following", [])
        next_cursor = following_data.get("next_cursor")

        # Extract metadata
        metadata = {
            "consumed_units": data.get("consumed_units"),
            "remaining_units": data.get("remaining_units"),
            "returned_items": data.get("returned_items"),
            "price_per_item": data.get("price_per_item"),
        }

        # Convert next_cursor to string if it's a number (0 means no more pages)
        if next_cursor is not None and next_cursor != 0:
            next_cursor = str(next_cursor)
        else:
            next_cursor = None

        if ids_only:
            return [str(uid) for uid in following_ids], next_cursor, metadata

        # Lookup full user details for the following IDs
        if following_ids:
            following = await self.get_user_details(following_ids)
        else:
            following = []

        return following, next_cursor, metadata

    async def lookup_users(
        self, user_ids: list[str], return_credits: bool = False
    ) -> list[dict] | tuple[list[dict], int]:
        """
        Lookup user details for a list of user IDs.

        Args:
            user_ids: List of Twitter user IDs to lookup
            return_credits: If True, returns tuple of (users, credits_used)

        Returns:
            If return_credits=False: List of user dicts
            If return_credits=True: Tuple of (users, credits_used)
        """
        if not user_ids:
            return ([], 0) if return_credits else []

        all_users = []
        total_credits = 0

        # Batch lookup - max 50 IDs per request
        for i in range(0, len(user_ids), 50):
            batch = user_ids[i : i + 50]
            params = {"user_ids": ",".join(batch)}
            data = await self.get("/twttr-api/users/lookup", params)
            all_users.extend(data.get("data", []))
            total_credits += data.get("consumed_units", 0)

        if return_credits:
            return all_users, total_credits
        return all_users

    async def search_tweets(
        self,
        query: str,
        search_type: str = "latest",
        cursor: str | None = None,
    ) -> tuple[list[dict], str | None, dict]:
        """
        Search for tweets matching a query.

        Args:
            query: Search query string (1-256 chars).
                   Supports operators like "bitcoin since:2025-01-01 from:elonmusk"
            search_type: "top" for popular tweets or "latest" for chronological (default)
            cursor: Pagination cursor from previous response

        Returns:
            Tuple of (tweets, next_cursor, metadata)
            - tweets: List of tweet dicts
            - next_cursor: Cursor for next page (None if no more)
            - metadata: Dict with consumed_units, remaining_units, etc.
        """
        params: dict[str, str | int] = {"query": query, "search_type": search_type}
        if cursor:
            params["cursor"] = cursor

        data = await self.get("/twttr-api/tweets/search", params)

        tweet_data = data.get("data", {})
        tweets = tweet_data.get("results", [])
        next_cursor = tweet_data.get("next_cursor")

        metadata = {
            "consumed_units": data.get("consumed_units"),
            "remaining_units": data.get("remaining_units"),
            "returned_items": data.get("returned_items"),
            "price_per_item": data.get("price_per_item"),
        }

        # Convert empty string cursor to None
        if not next_cursor:
            next_cursor = None

        return tweets, next_cursor, metadata

    async def lookup_tweets(
        self,
        tweet_ids: list[str],
        return_credits: bool = False,
    ) -> list[dict] | tuple[list[dict], int]:
        """
        Lookup tweets by their IDs.

        Args:
            tweet_ids: List of tweet IDs to lookup (max 100 per request)
            return_credits: If True, returns tuple of (tweets, credits_used)

        Returns:
            If return_credits=False: List of tweet dicts
            If return_credits=True: Tuple of (tweets, credits_used)
        """
        if not tweet_ids:
            return ([], 0) if return_credits else []

        all_tweets = []
        total_credits = 0

        # Batch lookup - max 100 IDs per request
        for i in range(0, len(tweet_ids), 100):
            batch = tweet_ids[i : i + 100]
            params = {"tweet_ids": ",".join(batch)}
            data = await self.get("/twttr-api/tweets/lookup", params)
            all_tweets.extend(data.get("data", []))
            total_credits += data.get("consumed_units", 0)

        if return_credits:
            return all_tweets, total_credits
        return all_tweets

    async def get_user_timeline(
        self,
        user_id: str,
        cursor: str | None = None,
    ) -> tuple[list[dict], str | None, dict]:
        """
        Get tweets from a user's timeline.

        Args:
            user_id: Twitter user ID (not screen_name)
            cursor: Pagination cursor from previous response

        Returns:
            Tuple of (tweets, next_cursor, metadata)
            - tweets: List of tweet dicts
            - next_cursor: Cursor for next page (None if no more)
            - metadata: Dict with consumed_units, remaining_units, etc.
        """
        params: dict[str, str] = {}
        if cursor:
            params["cursor"] = cursor

        data = await self.get(f"/twttr-api/users/{user_id}/timeline", params)

        tweet_data = data.get("data", {})
        tweets = tweet_data.get("results", [])
        next_cursor = tweet_data.get("next_cursor")

        metadata = {
            "consumed_units": data.get("consumed_units"),
            "remaining_units": data.get("remaining_units"),
            "returned_items": data.get("returned_items"),
            "price_per_item": data.get("price_per_item"),
        }

        # Convert empty string cursor to None
        if not next_cursor:
            next_cursor = None

        return tweets, next_cursor, metadata


# Backward compatibility alias
SynopticTwttrClient = SynopticClient
