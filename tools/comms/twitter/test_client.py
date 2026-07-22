import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from client import XClient


class StubXClient(XClient):
    def __init__(self, responses: list[dict[str, Any]]) -> None:
        super().__init__(api_key="unused")
        self.responses = responses
        self.requests: list[tuple[str, dict[str, Any] | None]] = []

    def _request(self, endpoint: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.requests.append((endpoint, params))
        if not self.responses:
            raise AssertionError("unexpected request")
        return self.responses.pop(0)


def test_search_uses_recent_search_endpoint_and_next_token() -> None:
    client = StubXClient(
        [
            {
                "data": [{"id": "1", "author_id": "10", "text": "one"}],
                "meta": {"next_token": "abc"},
            },
            {
                "data": [{"id": "2", "author_id": "10", "text": "two"}],
                "meta": {},
            },
        ]
    )

    tweets, _ = client.search_tweets("from:openai", limit=11)

    assert [tweet["tweet_id"] for tweet in tweets] == ["1", "2"]
    assert client.requests[0][0] == "/tweets/search/recent"
    assert client.requests[0][1]["query"] == "from:openai"
    assert client.requests[0][1]["sort_order"] == "recency"
    assert client.requests[0][1]["max_results"] == 11
    assert "pagination_token" not in client.requests[0][1]
    assert client.requests[1][1]["next_token"] == "abc"


def test_top_search_uses_relevancy_sort_order() -> None:
    client = StubXClient([{"data": [], "meta": {}}])

    client.search_tweets("openai", search_type="top", limit=10)

    assert client.requests[0][0] == "/tweets/search/recent"
    assert client.requests[0][1]["sort_order"] == "relevancy"


def test_full_archive_search_uses_all_endpoint() -> None:
    client = StubXClient([{"data": [], "meta": {}}])

    client.search_tweets("openai", search_type="all", limit=10)

    assert client.requests[0][0] == "/tweets/search/all"


def test_timeline_uses_specific_user_posts_endpoint() -> None:
    client = StubXClient(
        [
            {
                "data": {
                    "id": "10",
                    "username": "ada",
                    "name": "Ada",
                }
            },
            {
                "data": [{"id": "1", "author_id": "10", "text": "home"}],
                "meta": {},
                "includes": {"users": [{"id": "10", "username": "ada", "name": "Ada"}]},
            },
        ]
    )

    user, tweets, _ = client.get_timeline("ada", limit=1)

    assert user["user_id"] == "10"
    assert tweets[0]["screen_name"] == "ada"
    assert client.requests[0][0] == "/users/by/username/ada"
    assert client.requests[1][0] == "/users/10/tweets"
    assert client.requests[1][1]["max_results"] == 1
    assert client.requests[1][1]["pagination_token"] is None


def test_user_posts_keeps_authored_posts_endpoint() -> None:
    client = StubXClient(
        [
            {
                "data": {
                    "id": "10",
                    "username": "ada",
                    "name": "Ada",
                }
            },
            {
                "data": [{"id": "1", "author_id": "10", "text": "post"}],
                "meta": {},
            },
        ]
    )

    client.get_user_posts("ada", limit=5)

    assert client.requests[0][0] == "/users/by/username/ada"
    assert client.requests[1][0] == "/users/10/tweets"
    assert client.requests[1][1]["exclude"] == "retweets"
    assert client.requests[1][1]["pagination_token"] is None


def test_quote_tweets_uses_quote_tweets_endpoint_and_normalizes_authors() -> None:
    client = StubXClient(
        [
            {
                "data": [{"id": "2", "author_id": "20", "text": "quoted"}],
                "includes": {"users": [{"id": "20", "username": "grace", "name": "Grace"}]},
                "meta": {},
            }
        ]
    )

    tweets, _ = client.get_quote_tweets("1", limit=3)

    assert tweets[0]["tweet_id"] == "2"
    assert tweets[0]["screen_name"] == "grace"
    assert client.requests[0][0] == "/tweets/1/quote_tweets"
    assert client.requests[0][1]["max_results"] == 10


def test_retweeted_by_uses_retweeted_by_endpoint_and_normalizes_users() -> None:
    client = StubXClient(
        [
            {
                "data": [{"id": "20", "username": "grace", "name": "Grace"}],
                "meta": {},
            }
        ]
    )

    users, _ = client.get_retweeted_by("1", limit=3)

    assert users[0]["user_id"] == "20"
    assert users[0]["screen_name"] == "grace"
    assert client.requests[0][0] == "/tweets/1/retweeted_by"
    assert client.requests[0][1]["max_results"] == 3
