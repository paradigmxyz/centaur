"""X/Twitter API v2 client."""

from __future__ import annotations

from datetime import datetime
from typing import Any, Literal

import httpx

from centaur_sdk import secret

USER_FIELDS = (
    "created_at,description,entities,id,location,name,pinned_tweet_id,"
    "profile_image_url,protected,public_metrics,url,username,verified,verified_type"
)
TWEET_FIELDS = (
    "attachments,author_id,conversation_id,created_at,entities,geo,id,in_reply_to_user_id,"
    "lang,possibly_sensitive,public_metrics,referenced_tweets,reply_settings,source,text"
)
MEDIA_FIELDS = "alt_text,duration_ms,height,media_key,preview_image_url,public_metrics,type,url,width"
POLL_FIELDS = "duration_minutes,end_datetime,id,options,voting_status"
PLACE_FIELDS = "contained_within,country,country_code,full_name,geo,id,name,place_type"
DEFAULT_EXPANSIONS = "author_id,attachments.media_keys,attachments.poll_ids,geo.place_id"


class XClient:
    """Client for the X API v2."""

    BASE_URL = "https://api.x.com/2"

    def __init__(self, api_key: str | None = None, timeout: float = 30.0):
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _get_api_key(self) -> str:
        api_key = self._api_key or secret("X_API_KEY", "")
        if not api_key:
            raise RuntimeError("X_API_KEY not set.")
        return api_key

    def _request(self, endpoint: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        headers = {"Authorization": f"Bearer {self._get_api_key()}"}
        url = f"{self.BASE_URL}{endpoint}"
        try:
            response = self.client.get(url, params=self._clean_params(params), headers=headers)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"X API error: {e.response.status_code} - {e.response.text}") from e
        except httpx.RequestError as e:
            raise RuntimeError(f"X API request failed: {e}") from e

    @staticmethod
    def _clean_params(params: dict[str, Any] | None) -> dict[str, Any] | None:
        if not params:
            return None
        cleaned: dict[str, Any] = {}
        for key, value in params.items():
            if value is None:
                continue
            if isinstance(value, list):
                cleaned[key] = ",".join(str(v) for v in value)
            else:
                cleaned[key] = value
        return cleaned

    @staticmethod
    def _limit(value: int, minimum: int = 1, maximum: int = 100) -> int:
        return max(minimum, min(maximum, value))

    @staticmethod
    def _epoch_ms(value: str | None) -> int | None:
        if not value:
            return None
        try:
            return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000)
        except ValueError:
            return None

    def _normalize_user(self, user: dict[str, Any]) -> dict[str, Any]:
        metrics = user.get("public_metrics") or {}
        return {
            **user,
            "user_id": user.get("id"),
            "screen_name": user.get("username"),
            "followers_count": metrics.get("followers_count"),
            "following_count": metrics.get("following_count"),
            "statuses_count": metrics.get("tweet_count"),
            "listed_count": metrics.get("listed_count"),
            "is_blue_verified": bool(user.get("verified")),
            "website_url": user.get("url"),
        }

    def _users_by_id(self, includes: dict[str, Any] | None) -> dict[str, dict[str, Any]]:
        users = (includes or {}).get("users") or []
        return {str(user.get("id")): self._normalize_user(user) for user in users}

    def _normalize_tweet(
        self, tweet: dict[str, Any], includes: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        metrics = tweet.get("public_metrics") or {}
        author = self._users_by_id(includes).get(str(tweet.get("author_id")), {})
        created_at = tweet.get("created_at")
        return {
            **tweet,
            "tweet_id": tweet.get("id"),
            "published_at": self._epoch_ms(created_at),
            "author": author or None,
            "screen_name": author.get("screen_name") or author.get("username"),
            "like_count": metrics.get("like_count"),
            "retweet_count": metrics.get("retweet_count"),
            "reply_count": metrics.get("reply_count"),
            "quote_count": metrics.get("quote_count"),
            "bookmark_count": metrics.get("bookmark_count"),
            "view_count": metrics.get("impression_count"),
        }

    def _tweet_params(self, expansions: str | None = DEFAULT_EXPANSIONS) -> dict[str, Any]:
        return {
            "tweet.fields": TWEET_FIELDS,
            "user.fields": USER_FIELDS,
            "media.fields": MEDIA_FIELDS,
            "poll.fields": POLL_FIELDS,
            "place.fields": PLACE_FIELDS,
            "expansions": expansions,
        }

    def _paged(
        self,
        endpoint: str,
        data_key: str,
        limit: int,
        params: dict[str, Any] | None = None,
        max_page_size: int = 100,
        min_page_size: int = 1,
        token_param: str = "pagination_token",
    ) -> tuple[list[dict[str, Any]], dict[str, Any], dict[str, Any]]:
        params = dict(params or {})
        results: list[dict[str, Any]] = []
        includes: dict[str, Any] = {}
        meta: dict[str, Any] = {}
        token: str | None = None
        while len(results) < limit:
            page_size = self._limit(limit - len(results), minimum=min_page_size, maximum=max_page_size)
            request_params = {**params, "max_results": page_size, token_param: token}
            data = self._request(endpoint, request_params)
            meta = data.get("meta") or {}
            for key, value in (data.get("includes") or {}).items():
                includes.setdefault(key, []).extend(value)
            results.extend(data.get(data_key) or [])
            token = meta.get("next_token")
            if not token or not data.get(data_key):
                break
        return results[:limit], meta, includes

    def get_user(self, handle: str) -> dict[str, Any] | None:
        """Get a user profile by username/handle."""
        data = self._request(
            f"/users/by/username/{handle.lstrip('@')}",
            {"user.fields": USER_FIELDS},
        )
        user = data.get("data")
        return self._normalize_user(user) if user else None

    def get_user_by_id(self, user_id: str) -> dict[str, Any] | None:
        """Get a user profile by X user ID."""
        data = self._request(f"/users/{user_id}", {"user.fields": USER_FIELDS})
        user = data.get("data")
        return self._normalize_user(user) if user else None

    def lookup_users(self, ids: list[str]) -> list[dict[str, Any]]:
        """Lookup users by IDs."""
        data = self._request("/users", {"ids": ids, "user.fields": USER_FIELDS})
        return [self._normalize_user(user) for user in data.get("data") or []]

    def lookup_users_by_usernames(self, usernames: list[str]) -> list[dict[str, Any]]:
        """Lookup users by usernames/handles."""
        names = [name.lstrip("@") for name in usernames]
        data = self._request("/users/by", {"usernames": names, "user.fields": USER_FIELDS})
        return [self._normalize_user(user) for user in data.get("data") or []]

    def get_followers(
        self, handle: str, limit: int = 100, ids_only: bool = False
    ) -> tuple[list[dict[str, Any]] | list[str], dict[str, Any]]:
        """Get followers for a user."""
        user = self.get_user(handle)
        if not user:
            return [], {}
        params = {"user.fields": USER_FIELDS}
        followers, meta, _ = self._paged(f"/users/{user['user_id']}/followers", "data", limit, params)
        normalized = [self._normalize_user(item) for item in followers]
        if ids_only:
            return [item["user_id"] for item in normalized if item.get("user_id")], meta
        return normalized, meta

    def get_following(
        self, handle: str, limit: int = 100, ids_only: bool = False
    ) -> tuple[list[dict[str, Any]] | list[str], dict[str, Any]]:
        """Get users followed by a user."""
        user = self.get_user(handle)
        if not user:
            return [], {}
        params = {"user.fields": USER_FIELDS}
        following, meta, _ = self._paged(f"/users/{user['user_id']}/following", "data", limit, params)
        normalized = [self._normalize_user(item) for item in following]
        if ids_only:
            return [item["user_id"] for item in normalized if item.get("user_id")], meta
        return normalized, meta

    def search_tweets(
        self,
        query: str,
        search_type: Literal["latest", "top", "recent", "all"] = "latest",
        limit: int = 20,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Search recent or full-archive posts. Use search_type='all' for full archive."""
        endpoint = "/tweets/search/all" if search_type == "all" else "/tweets/search/recent"
        params = {
            **self._tweet_params(),
            "query": query,
            "sort_order": "relevancy" if search_type == "top" else "recency",
        }
        tweets, meta, includes = self._paged(
            endpoint,
            "data",
            limit,
            params,
            max_page_size=100,
            min_page_size=10,
            token_param="next_token",
        )
        return [self._normalize_tweet(tweet, includes) for tweet in tweets[:limit]], meta

    def lookup_tweets(self, ids: list[str]) -> list[dict[str, Any]]:
        """Lookup posts by IDs."""
        data = self._request("/tweets", {"ids": ids, **self._tweet_params()})
        includes = data.get("includes")
        return [self._normalize_tweet(tweet, includes) for tweet in data.get("data") or []]

    def get_tweet(self, tweet_id: str) -> dict[str, Any] | None:
        """Lookup a single post by ID."""
        data = self._request(f"/tweets/{tweet_id}", self._tweet_params())
        tweet = data.get("data")
        return self._normalize_tweet(tweet, data.get("includes")) if tweet else None

    def get_user_posts(
        self, handle: str, limit: int = 20
    ) -> tuple[dict[str, Any] | None, list[dict[str, Any]], dict[str, Any] | None]:
        """Get a user's recent posts by handle."""
        user = self.get_user(handle)
        if not user:
            return None, [], None
        params = {**self._tweet_params(), "exclude": "retweets"}
        tweets, meta, includes = self._paged(f"/users/{user['user_id']}/tweets", "data", limit, params)
        return user, [self._normalize_tweet(tweet, includes) for tweet in tweets], meta

    def get_timeline(
        self, handle: str, limit: int = 20
    ) -> tuple[dict[str, Any] | None, list[dict[str, Any]], dict[str, Any] | None]:
        """Get a specific user's authored posts timeline by handle."""
        return self.get_user_posts(handle, limit=limit)

    def get_mentions(self, handle: str, limit: int = 20) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Get recent mentions for a user."""
        user = self.get_user(handle)
        if not user:
            return [], {}
        tweets, meta, includes = self._paged(
            f"/users/{user['user_id']}/mentions", "data", limit, self._tweet_params()
        )
        return [self._normalize_tweet(tweet, includes) for tweet in tweets], meta

    def get_liking_users(
        self, tweet_id: str, limit: int = 100
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Get users who liked a post."""
        users, meta, _ = self._paged(
            f"/tweets/{tweet_id}/liking_users", "data", limit, {"user.fields": USER_FIELDS}
        )
        return [self._normalize_user(user) for user in users], meta

    def get_retweeted_by(
        self, tweet_id: str, limit: int = 100
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Get users who reposted a post."""
        users, meta, _ = self._paged(
            f"/tweets/{tweet_id}/retweeted_by", "data", limit, {"user.fields": USER_FIELDS}
        )
        return [self._normalize_user(user) for user in users], meta

    def get_list_tweets(
        self, list_id: str, limit: int = 20
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Get posts from a List."""
        tweets, meta, includes = self._paged(f"/lists/{list_id}/tweets", "data", limit, self._tweet_params())
        return [self._normalize_tweet(tweet, includes) for tweet in tweets], meta

    def get_list_members(
        self, list_id: str, limit: int = 100
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Get members of a List."""
        users, meta, _ = self._paged(
            f"/lists/{list_id}/members", "data", limit, {"user.fields": USER_FIELDS}
        )
        return [self._normalize_user(user) for user in users], meta

    def get_list_followers(
        self, list_id: str, limit: int = 100
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Get followers of a List."""
        users, meta, _ = self._paged(
            f"/lists/{list_id}/followers", "data", limit, {"user.fields": USER_FIELDS}
        )
        return [self._normalize_user(user) for user in users], meta

    def get_usage(self) -> dict[str, str]:
        """Return a note about X API usage reporting."""
        return {"message": "X API v2 does not expose a general usage endpoint for bearer tokens."}

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> XClient:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


def _client() -> XClient:
    return XClient(api_key=secret("X_API_KEY", ""))
