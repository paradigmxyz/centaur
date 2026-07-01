"""Synoptic Twitter API client."""

import asyncio
import logging
import re

import httpx

from centaur_sdk import secret

from .sdk import TwitterClient

logger = logging.getLogger(__name__)

FXTWITTER_BASE = "https://api.fxtwitter.com"
SYNOPTIC_BASE_URL = "https://api.synoptic.com"
_ARTICLE_URL_RE = re.compile(r"x\.com/i/article/(\d+)")
_TWEET_URL_RE = re.compile(r"(?:x|twitter)\.com/([^/]+)/status/(\d+)")


def _render_article_blocks(blocks: list[dict]) -> str:
    """Render Draft.js-style article blocks to markdown text."""
    lines: list[str] = []
    for block in blocks:
        text = block.get("text", "")
        btype = block.get("type", "unstyled")
        if not text and btype != "atomic":
            lines.append("")
            continue
        if btype == "header-one":
            lines.append(f"# {text}")
        elif btype == "header-two":
            lines.append(f"## {text}")
        elif btype == "header-three":
            lines.append(f"### {text}")
        elif btype == "unordered-list-item":
            lines.append(f"• {text}")
        elif btype == "ordered-list-item":
            lines.append(f"- {text}")
        elif btype == "blockquote":
            lines.append(f"> {text}")
        elif btype == "atomic":
            if text:
                lines.append(text)
        else:
            lines.append(text)
    return "\n".join(lines)


class SynopticClient:
    """Sync wrapper around the embedded TwitterClient SDK."""

    def __init__(
        self,
        api_key: str | None = None,
        base_url: str | None = None,
    ):
        self._api_key = api_key or secret("SYNOPTIC_API_KEY", "")
        url = base_url or SYNOPTIC_BASE_URL
        if url and not url.startswith(("http://", "https://")):
            url = f"https://{url}"
        self._base_url = url

    def _make_client(self) -> TwitterClient:
        return TwitterClient(api_key=self._api_key, base_url=self._base_url)

    def _run(self, coro):
        return asyncio.run(coro)

    @staticmethod
    def _has_article_url(tweet: dict) -> bool:
        """Check if a tweet links to an X article."""
        return any(_ARTICLE_URL_RE.search(url) for url in tweet.get("urls") or [])

    @staticmethod
    def _fetch_article_via_fxtwitter(screen_name: str, tweet_id: str) -> dict | None:
        """Fetch article content for a tweet via the fxtwitter API."""
        try:
            resp = httpx.get(
                f"{FXTWITTER_BASE}/{screen_name}/status/{tweet_id}",
                timeout=15.0,
            )
            resp.raise_for_status()
            data = resp.json()
            article = data.get("tweet", {}).get("article")
            if not article:
                return None
            blocks = article.get("content", {}).get("blocks", [])
            return {
                "title": article.get("title", ""),
                "preview_text": article.get("preview_text", ""),
                "text": _render_article_blocks(blocks) if blocks else "",
            }
        except Exception:
            logger.warning("Failed to fetch article for %s/%s", screen_name, tweet_id)
            return None

    def _expand_article(self, tweet: dict) -> dict:
        """If tweet links to an X article, fetch and attach the article content."""
        if not self._has_article_url(tweet):
            return tweet
        screen_name = tweet.get("screen_name", "")
        tweet_id = tweet.get("tweet_id", "")
        if not screen_name or not tweet_id:
            return tweet
        article = self._fetch_article_via_fxtwitter(screen_name, tweet_id)
        if article:
            tweet = {**tweet, "article": article}
        return tweet

    def get_user(self, handle: str) -> dict | None:
        """Get user profile by handle."""

        async def _do():
            async with self._make_client() as client:
                return await client.get_user_by_screen_name(handle)

        return self._run(_do())

    def get_followers(
        self, handle: str, limit: int = 100, ids_only: bool = False
    ) -> tuple[list, dict]:
        """Get followers with pagination."""

        async def _do():
            async with self._make_client() as client:
                all_followers = []
                cursor = None
                while len(all_followers) < limit:
                    batch_size = min(1000, limit - len(all_followers))
                    followers, cursor, meta = await client.get_followers(
                        handle, cursor=cursor, ids_only=ids_only, max_results=batch_size
                    )
                    all_followers.extend(followers)
                    if not cursor:
                        break
                return all_followers[:limit], meta

        return self._run(_do())

    def get_following(
        self, handle: str, limit: int = 100, ids_only: bool = False
    ) -> tuple[list, dict]:
        """Get following with pagination."""

        async def _do():
            async with self._make_client() as client:
                all_following = []
                cursor = None
                while len(all_following) < limit:
                    batch_size = min(1000, limit - len(all_following))
                    following, cursor, meta = await client.get_following(
                        handle, cursor=cursor, ids_only=ids_only, max_results=batch_size
                    )
                    all_following.extend(following)
                    if not cursor:
                        break
                return all_following[:limit], meta

        return self._run(_do())

    def lookup_users(self, ids: list[str]) -> list[dict]:
        """Lookup users by IDs."""

        async def _do():
            async with self._make_client() as client:
                return await client.lookup_users(ids)

        return self._run(_do())

    def search_tweets(
        self, query: str, search_type: str = "latest", limit: int = 20
    ) -> tuple[list, dict]:
        """Search tweets by keyword or advanced query (e.g. 'ethereum', 'from:vitalik ETH'). Use get_timeline instead if you just want a user's recent tweets."""

        async def _do():
            async with self._make_client() as client:
                all_tweets = []
                cursor = None
                while len(all_tweets) < limit:
                    tweets, cursor, meta = await client.search_tweets(
                        query, search_type=search_type, cursor=cursor
                    )
                    all_tweets.extend(tweets)
                    if not cursor or not tweets:
                        break
                return all_tweets[:limit], meta

        return self._run(_do())

    def lookup_tweets(self, ids: list[str]) -> list[dict]:
        """Lookup tweets by IDs. Automatically expands inline X article content."""

        async def _do():
            async with self._make_client() as client:
                return await client.lookup_tweets(ids)

        tweets = self._run(_do())
        return [self._expand_article(t) for t in tweets]

    def get_article(self, tweet_url: str) -> dict:
        """Fetch an X/Twitter long-form article. Pass the tweet URL or article URL (x.com/i/article/...)."""
        m = _TWEET_URL_RE.search(tweet_url)
        if m:
            screen_name, tweet_id = m.group(1), m.group(2)
        else:
            # Try to resolve article URL by looking up the tweet that contains it
            am = _ARTICLE_URL_RE.search(tweet_url)
            if am:
                return {"error": "Please provide the tweet URL, not the article URL directly."}
            return {"error": f"Could not parse tweet URL: {tweet_url}"}

        article = self._fetch_article_via_fxtwitter(screen_name, tweet_id)
        if not article:
            return {"error": "No article found for this tweet."}
        article["tweet_url"] = f"https://x.com/{screen_name}/status/{tweet_id}"
        article["author"] = screen_name
        return article

    def get_timeline(self, handle: str, limit: int = 20) -> tuple[dict | None, list, dict | None]:
        """Get a user's recent tweets by handle. This is the best method for 'last N tweets by @user' requests."""

        async def _do():
            async with self._make_client() as client:
                user = await client.get_user_by_screen_name(handle)
                if not user:
                    return None, [], None

                user_id = user.get("user_id")
                all_tweets = []
                cursor = None
                while len(all_tweets) < limit:
                    tweets, cursor, meta = await client.get_user_timeline(user_id, cursor=cursor)
                    all_tweets.extend(tweets)
                    if not cursor or not tweets:
                        break
                return user, all_tweets[:limit], meta

        return self._run(_do())

    def get_usage(self):
        """Check API credit usage."""

        async def _do():
            async with self._make_client() as client:
                await client.get_user_by_screen_name("twitter")
                return client.get_usage()

        return self._run(_do())


def _client() -> SynopticClient:
    return SynopticClient()
