import sys
from pathlib import Path

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from client import XClient, XCreditsDepletedError

USER = {"id": "10", "screen_name": "ada", "name": "Ada", "followers": 42}
POST = {
    "id": "1",
    "text": "A complete public post",
    "author": USER,
    "created_timestamp": 1700000000,
    "likes": 7,
    "reposts": 2,
    "article": {"title": "An article", "content": {"blocks": [{"text": "Body"}]}},
    "media": {
        "all": [
            {
                "type": "video",
                "url": "https://example.com/video.mp4",
                "duration": 1.5,
                "altText": "A video",
                "formats": [{"url": "https://example.com/video.mp4", "container": "mp4"}],
            }
        ]
    },
    "quote": {"id": "2", "text": "quoted", "author": USER},
    "replying_to": {"status": "3"},
}


def make_client(handler):
    client = XClient(api_key="test-token")
    client._client = httpx.Client(transport=httpx.MockTransport(handler))
    return client


def test_credit_fallback_preserves_post_content_and_never_sends_credentials():
    requests = []

    def handler(request):
        requests.append(request)
        if request.url.host == "api.x.com":
            assert request.headers["Authorization"] == "Bearer test-token"
            return httpx.Response(402, json={"title": "CreditsDepleted"})
        assert request.url.host == "api.fxtwitter.com"
        assert request.url.path == "/2/status/1"
        assert "Authorization" not in request.headers
        return httpx.Response(200, json={"code": 200, "status": POST})

    with make_client(handler) as client:
        tweet = client.get_tweet("1")
        assert tweet["tweet_id"] == "1"
        assert tweet["text"] == POST["text"]
        assert tweet["screen_name"] == "ada"
        assert tweet["author"]["followers_count"] == 42
        assert tweet["published_at"] == 1700000000000
        assert tweet["like_count"] == 7
        assert tweet["retweet_count"] == 2
        assert tweet["article"] == POST["article"]
        assert tweet["media"][0]["duration_ms"] == 1500
        assert tweet["media"][0]["alt_text"] == "A video"
        assert tweet["media"][0]["variants"][0]["content_type"] == "video/mp4"
        assert tweet["referenced_tweets"][0] == {"type": "replied_to", "id": "3"}
        assert tweet["referenced_tweets"][1]["tweet"]["text"] == "quoted"
        assert tweet["provider"] == "fxtwitter"
        client.get_tweet("1")
    assert len(requests) == 3  # Credit exhaustion is remembered for this client only.


@pytest.mark.parametrize("status", [401, 403, 404, 429, 500])
def test_other_x_errors_do_not_trigger_fallback(status):
    def handler(request):
        assert request.url.host == "api.x.com"
        return httpx.Response(status, json={"title": "Unrelated failure"})

    with (
        make_client(handler) as client,
        pytest.raises(RuntimeError, match=f"X API error: {status}"),
    ):
        client.get_tweet("1")


def test_paid_success_does_not_contact_fallback():
    def handler(request):
        assert request.url.host == "api.x.com"
        return httpx.Response(200, json={"data": {"id": "1", "text": "paid"}})

    with make_client(handler) as client:
        assert client.get_tweet("1")["text"] == "paid"


def test_mid_pagination_exhaustion_restarts_search_without_mixing_cursors():
    calls = []

    def handler(request):
        calls.append(request)
        if request.url.host == "api.x.com":
            if "next_token" in request.url.params:
                return httpx.Response(402)
            return httpx.Response(
                200, json={"data": [{"id": "old"}], "meta": {"next_token": "paid"}}
            )
        assert request.url.params["feed"] == "top"
        assert request.url.params["q"] == "from:ada"
        assert "next_token" not in request.url.params
        if "cursor" not in request.url.params:
            return httpx.Response(200, json={"results": [POST], "cursor": {"bottom": "fx"}})
        assert request.url.params["cursor"] == "fx"
        return httpx.Response(200, json={"results": [{**POST, "id": "2"}]})

    with make_client(handler) as client:
        posts, meta = client.search_tweets("from:ada", search_type="top", limit=3)
    assert [post["id"] for post in posts] == ["1", "2"]
    assert meta == {"provider": "fxtwitter", "result_count": 2}
    assert len(calls) == 4


@pytest.mark.parametrize(
    "method", ["get_followers", "get_following", "get_user_posts", "get_timeline"]
)
def test_profile_based_reads_fall_back(method):
    def handler(request):
        if request.url.host == "api.x.com":
            return httpx.Response(402)
        assert "Authorization" not in request.headers
        if request.url.path == "/2/profile/ada":
            return httpx.Response(200, json={"user": USER})
        if method in {"get_followers", "get_following"}:
            assert request.url.path == f"/2/profile/ada/{method.removeprefix('get_')}"
            return httpx.Response(200, json={"results": [USER]})
        assert request.url.path == "/2/profile/ada/statuses"
        assert request.url.params["with_replies"] == "true"
        return httpx.Response(200, json={"results": [POST, {**POST, "reposted_by": USER}]})

    with make_client(handler) as client:
        if method in {"get_followers", "get_following"}:
            users, meta = getattr(client, method)("@ada", ids_only=True)
            assert users == ["10"]
        else:
            user, posts, meta = getattr(client, method)("@ada")
            assert user["screen_name"] == "ada"
            assert [post["id"] for post in posts] == ["1"]
        assert meta["result_count"] == 1


@pytest.mark.parametrize(
    "method,args",
    [
        ("lookup_tweets", (["1", "2"],)),
        ("lookup_users_by_usernames", (["ada", "bob"],)),
    ],
)
def test_batch_reads_fall_back(method, args):
    def handler(request):
        if request.url.host == "api.x.com":
            return httpx.Response(402)
        identifier = request.url.path.split("/")[-1]
        if method == "lookup_tweets":
            return httpx.Response(200, json={"tweet": {**POST, "id": identifier}})
        return httpx.Response(200, json={"user": {**USER, "screen_name": identifier}})

    with make_client(handler) as client:
        result = getattr(client, method)(*args)
        key = "tweet_id" if method == "lookup_tweets" else "screen_name"
        assert [item[key] for item in result] == args[0]


@pytest.mark.parametrize(
    "payload,message",
    [
        ({"code": 404, "results": []}, "temporarily unavailable"),
        ({"code": 503}, "fallback error"),
        ({"code": 429}, "fallback error"),
        ({"message": "PRIVATE_TWEET"}, "fallback error"),
        ({}, "missing results"),
        ([], "invalid response"),
    ],
)
def test_fallback_errors_are_not_empty_successes(payload, message):
    def handler(request):
        if request.url.host == "api.x.com":
            return httpx.Response(402)
        return httpx.Response(200, json=payload)

    with make_client(handler) as client, pytest.raises(RuntimeError, match=message):
        client.search_tweets("test")


def test_unsupported_full_archive_and_usage_remain_explicit_errors():
    def handler(request):
        assert request.url.host == "api.x.com"
        return httpx.Response(402)

    with make_client(handler) as client:
        with pytest.raises(XCreditsDepletedError, match="full-archive"):
            client.search_tweets("test", search_type="all")
        with pytest.raises(XCreditsDepletedError, match="HTTP 402"):
            client.get_usage()
        with pytest.raises(XCreditsDepletedError, match="supported fallback"):
            client.get_list_members("1")


def test_network_failure_does_not_trigger_fallback():
    def handler(request):
        assert request.url.host == "api.x.com"
        raise httpx.ConnectError("offline", request=request)

    with make_client(handler) as client, pytest.raises(RuntimeError, match="X API request failed"):
        client.get_tweet("1")


def test_fallback_http_denial_is_visible():
    def handler(request):
        if request.url.host == "api.x.com":
            return httpx.Response(402)
        return httpx.Response(403, text="upstream challenge")

    with make_client(handler) as client, pytest.raises(RuntimeError, match="403 Forbidden"):
        client.get_tweet("1")


def test_fallback_pagination_is_bounded():
    calls = []

    def handler(request):
        if request.url.host == "api.x.com":
            return httpx.Response(402)
        calls.append(request)
        assert int(request.url.params["count"]) <= 20
        return httpx.Response(
            200,
            json={
                "results": [{**POST, "id": str(len(calls))}],
                "cursor": {"bottom": str(len(calls))},
            },
        )

    with make_client(handler) as client:
        posts, meta = client.search_tweets("test", limit=1000)
    assert len(calls) == len(posts) == 10
    assert meta["next_token"] == "10"
