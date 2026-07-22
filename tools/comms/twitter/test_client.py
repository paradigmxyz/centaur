import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from client import DEFAULT_EXPANSIONS, MEDIA_FIELDS, XAPIResponseError, XClient


class StubXClient(XClient):
    def __init__(self, responses: list[dict[str, Any]]) -> None:
        super().__init__(api_key="unused")
        self.responses = responses
        self.requests: list[tuple[str, dict[str, Any] | None]] = []

    def _request(self, endpoint: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.requests.append((endpoint, params))
        if not self.responses:
            raise AssertionError("unexpected request")
        return self._validate_response(self.responses.pop(0))


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


def test_pagination_metadata_describes_returned_results() -> None:
    client = StubXClient(
        [
            {
                "data": [{"id": "1"}, {"id": "2"}],
                "meta": {
                    "newest_id": "1",
                    "oldest_id": "2",
                    "result_count": 2,
                    "next_token": "abc",
                },
            }
        ]
    )

    tweets, meta = client.search_tweets("openai", limit=1)

    assert [tweet["tweet_id"] for tweet in tweets] == ["1"]
    assert meta == {
        "newest_id": "1",
        "oldest_id": "2",
        "result_count": 1,
        "next_token": "abc",
    }


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


def test_get_tweet_promotes_long_form_text_and_entities() -> None:
    client = StubXClient(
        [
            {
                "data": {
                    "id": "1",
                    "text": "A long post that stops early, and",
                    "entities": {"hashtags": [{"tag": "short"}]},
                    "note_tweet": {
                        "text": "A long post that stops early, and then continues to the end.",
                        "entities": {"hashtags": [{"tag": "complete"}]},
                    },
                }
            }
        ]
    )

    tweet = client.get_tweet("1")

    assert tweet is not None
    assert tweet["text"] == "A long post that stops early, and then continues to the end."
    assert tweet["entities"] == {"hashtags": [{"tag": "complete"}]}
    assert tweet["note_tweet"]["text"].endswith("continues to the end.")
    assert "note_tweet" in client.requests[0][1]["tweet.fields"].split(",")


def test_get_tweet_keeps_standard_text_without_note_tweet() -> None:
    client = StubXClient([{"data": {"id": "1", "text": "A standard post."}}])

    tweet = client.get_tweet("1")

    assert tweet is not None
    assert tweet["text"] == "A standard post."
    assert tweet["entities"] is None


def test_tweet_fields_request_articles_video_variants_and_reference_expansions() -> None:
    client = StubXClient([{"data": {"id": "1", "text": "post"}}])

    client.get_tweet("1")

    params = client.requests[0][1]
    assert "article" in params["tweet.fields"].split(",")
    assert "variants" in MEDIA_FIELDS.split(",")
    assert "article.cover_media" in DEFAULT_EXPANSIONS.split(",")
    assert "article.media_entities" in DEFAULT_EXPANSIONS.split(",")
    assert "referenced_tweets.id" in DEFAULT_EXPANSIONS.split(",")
    assert "referenced_tweets.id.author_id" in DEFAULT_EXPANSIONS.split(",")


def test_get_tweet_hydrates_media_poll_place_and_referenced_tweet() -> None:
    client = StubXClient(
        [
            {
                "data": {
                    "id": "1",
                    "author_id": "10",
                    "text": "quote",
                    "attachments": {"media_keys": ["3_1"], "poll_ids": ["30"]},
                    "geo": {"place_id": "40"},
                    "referenced_tweets": [{"type": "quoted", "id": "2"}],
                },
                "includes": {
                    "users": [
                        {"id": "10", "username": "ada"},
                        {"id": "20", "username": "grace"},
                    ],
                    "media": [
                        {
                            "media_key": "3_1",
                            "type": "video",
                            "variants": [{"url": "https://video.example/post.mp4"}],
                        },
                        {"media_key": "3_2", "type": "photo"},
                    ],
                    "polls": [{"id": "30", "voting_status": "open"}],
                    "places": [{"id": "40", "full_name": "London"}],
                    "tweets": [
                        {
                            "id": "2",
                            "author_id": "20",
                            "text": "original",
                            "attachments": {"media_keys": ["3_2"]},
                        }
                    ],
                },
            }
        ]
    )

    tweet = client.get_tweet("1")

    assert tweet is not None
    assert tweet["media"][0]["variants"][0]["url"].endswith("post.mp4")
    assert tweet["polls"] == [{"id": "30", "voting_status": "open"}]
    assert tweet["place"] == {"id": "40", "full_name": "London"}
    referenced = tweet["referenced_tweets"][0]["tweet"]
    assert referenced["text"] == "original"
    assert referenced["screen_name"] == "grace"
    assert referenced["media"] == [{"media_key": "3_2", "type": "photo"}]


def test_blue_verification_is_distinct_from_organization_verification() -> None:
    client = StubXClient(
        [
            {"data": {"id": "1", "username": "blue", "verified_type": "blue"}},
            {
                "data": {
                    "id": "2",
                    "username": "business",
                    "verified": True,
                    "verified_type": "business",
                }
            },
        ]
    )

    blue = client.get_user("blue")
    business = client.get_user("business")

    assert blue is not None and blue["is_blue_verified"] is True
    assert business is not None and business["is_blue_verified"] is False


def test_partial_api_errors_are_not_silently_dropped() -> None:
    client = StubXClient(
        [
            {
                "data": [{"id": "1", "text": "valid"}],
                "errors": [
                    {
                        "value": "missing",
                        "title": "Not Found Error",
                        "detail": "Could not find post missing.",
                    }
                ],
            }
        ]
    )

    try:
        client.lookup_tweets(["1", "missing"])
    except XAPIResponseError as error:
        assert error.data == [{"id": "1", "text": "valid"}]
        assert error.errors[0]["value"] == "missing"
        assert "Could not find post missing" in str(error)
    else:
        raise AssertionError("partial X API errors must be surfaced")


def test_batch_lookup_chunks_requests_at_api_limit() -> None:
    ids = [str(index) for index in range(101)]
    client = StubXClient(
        [
            {"data": [{"id": value, "text": value} for value in ids[:100]]},
            {"data": [{"id": ids[100], "text": ids[100]}]},
        ]
    )

    tweets = client.lookup_tweets(ids)

    assert len(tweets) == 101
    assert client.requests[0][1]["ids"] == ids[:100]
    assert client.requests[1][1]["ids"] == ids[100:]


def test_empty_batch_lookup_makes_no_request() -> None:
    client = StubXClient([])

    assert client.lookup_tweets([]) == []
    assert client.lookup_users([]) == []
    assert client.lookup_users_by_usernames([]) == []
    assert client.requests == []


def test_usage_calls_real_api_endpoint() -> None:
    client = StubXClient([{"data": {"project_usage": 42}}])

    usage = client.get_usage()

    assert usage == {"data": {"project_usage": 42}}
    assert client.requests == [("/usage/tweets", None)]


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
