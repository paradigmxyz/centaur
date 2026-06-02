"""CoinDesk RSS client."""

import contextlib
import html
import json
import re
from urllib.parse import urlparse

import feedparser


class CoinDeskClient:
    """Client for CoinDesk RSS feed."""

    RSS_URL = "https://www.coindesk.com/arc/outboundfeeds/rss/"
    USER_AGENT = (
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
        "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    )

    def __init__(self, timeout: float = 30.0):
        self.timeout = timeout

    def _fetch_feed(self) -> list[dict]:
        """Fetch and parse RSS feed using httpx with browser-like headers."""
        import httpx

        headers = {
            "User-Agent": self.USER_AGENT,
            "Accept": "application/rss+xml, application/xml, text/xml, */*",
            "Accept-Language": "en-US,en;q=0.9",
            "Accept-Encoding": "gzip, deflate",
            "Referer": "https://www.coindesk.com/",
        }
        try:
            with httpx.Client(timeout=self.timeout, follow_redirects=True) as client:
                response = client.get(self.RSS_URL, headers=headers)
                response.raise_for_status()
                content = response.text
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"Failed to fetch feed: HTTP {e.response.status_code}") from e
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}") from e

        feed = feedparser.parse(content)
        if feed.bozo and not feed.entries:
            fallback = self._parse_json_or_html_articles(content)
            if fallback:
                return fallback
            bozo_msg = str(feed.bozo_exception) if hasattr(feed, "bozo_exception") else "unknown"
            raise RuntimeError(
                f"Failed to parse feed ({bozo_msg}). "
                f"HTTP {response.status_code}, body length {len(content)}"
            )

        return self._parse_entries(feed.entries)

    def _parse_json_or_html_articles(self, content: str) -> list[dict]:
        """Best-effort fallback for CoinDesk 200 responses that are not RSS."""
        stripped = content.lstrip()
        if stripped.startswith(("{", "[")):
            with contextlib.suppress(Exception):
                return self._parse_json_articles(json.loads(content))
        return self._parse_html_articles(content)

    def _parse_json_articles(self, payload) -> list[dict]:
        candidates = []
        if isinstance(payload, list):
            candidates = payload
        elif isinstance(payload, dict):
            for key in ("articles", "items", "data", "results"):
                value = payload.get(key)
                if isinstance(value, list):
                    candidates = value
                    break
        articles = []
        for item in candidates:
            if not isinstance(item, dict):
                continue
            title = item.get("title") or item.get("headline") or item.get("name") or ""
            link = item.get("url") or item.get("link") or item.get("permalink") or ""
            summary = item.get("summary") or item.get("description") or item.get("excerpt") or ""
            normalized_link = self._coindesk_url(str(link))
            if title and normalized_link:
                articles.append(
                    {
                        "title": str(title),
                        "link": normalized_link,
                        "published": str(item.get("published") or item.get("date") or ""),
                        "summary": str(summary),
                        "author": str(item.get("author") or ""),
                        "tags": [],
                    }
                )
        return articles

    def _parse_html_articles(self, content: str) -> list[dict]:
        articles = []
        seen: set[tuple[str, str]] = set()
        for match in re.finditer(
            r"""<a[^>]+href=(["'])(.*?)\1[^>]*>(.*?)</a>""",
            content,
            re.I | re.S,
        ):
            link = self._coindesk_url(html.unescape(match.group(2)).strip())
            if not link:
                continue
            title = re.sub(r"<[^>]+>", " ", match.group(3))
            title = re.sub(r"\s+", " ", html.unescape(title)).strip()
            if not title:
                continue
            key = (title, link)
            if key in seen:
                continue
            seen.add(key)
            articles.append(
                {
                    "title": title,
                    "link": link,
                    "published": "",
                    "summary": "",
                    "author": "",
                    "tags": [],
                }
            )
        return articles

    @staticmethod
    def _coindesk_url(link: str) -> str:
        """Return normalized CoinDesk article URL or empty string for non-CoinDesk links."""
        link = link.strip()
        if not link:
            return ""
        if link.startswith("/"):
            link = f"https://www.coindesk.com{link}"
        parsed = urlparse(link)
        if parsed.netloc not in {"www.coindesk.com", "coindesk.com"}:
            return ""
        if "/20" not in parsed.path:
            return ""
        return link

    def _parse_entries(self, entries: list) -> list[dict]:
        """Parse feed entries into article dicts."""
        articles = []
        for entry in entries:
            articles.append(
                {
                    "title": entry.get("title", ""),
                    "link": entry.get("link", ""),
                    "published": entry.get("published", ""),
                    "summary": entry.get("summary", ""),
                    "author": entry.get("author", ""),
                    "tags": [tag.term for tag in entry.get("tags", [])]
                    if entry.get("tags")
                    else [],
                }
            )
        return articles

    def news(self, limit: int = 20) -> list[dict]:
        """Get latest news articles."""
        articles = self._fetch_feed()
        return articles[:limit]

    def search(self, query: str, limit: int = 20) -> list[dict]:
        """Search news articles by keyword."""
        articles = self._fetch_feed()
        query_lower = query.lower()
        filtered = [
            article
            for article in articles
            if query_lower in article["title"].lower()
            or query_lower in article["summary"].lower()
            or any(query_lower in tag.lower() for tag in article["tags"])
        ]
        return filtered[:limit]


def _client() -> CoinDeskClient:
    return CoinDeskClient()
