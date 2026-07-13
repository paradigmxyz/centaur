import base64
import email.message
import json

import pytest
from slack.client import SlackAuthError, SlackClient, SlackRateLimitError
from slack_sdk.errors import SlackApiError


class _FakeSlackResponse(dict):
    def __init__(
        self, *, error: str = "ratelimited", headers: dict | None = None, status_code: int = 429
    ) -> None:
        super().__init__(error=error)
        self.headers = headers or {}
        self.status_code = status_code


class _FakeWebClient:
    def __init__(self) -> None:
        self.last_kwargs = None
        self.history_calls: list[dict] = []
        self.history_pages: list[dict] = []
        self.reply_calls: list[dict] = []
        self.reply_pages: list[dict] = []
        self.open_calls: list[dict] = []
        self.open_response: dict = {"channel": {"id": "D123"}}
        self.users_calls: list[dict] = []
        self.users_pages: list[dict] = []
        self.list_calls: list[dict] = []
        self.list_pages: list[dict] = []
        self.user_conversations_calls: list[dict] = []
        self.user_conversations_pages: list[dict] = []
        self.api_calls: list[tuple[str, dict]] = []
        self.user_info_response: dict | None = None
        self.user_profile_response: dict | None = None
        self.user_profile_calls: list[dict] = []
        self.upload_exception: Exception | None = None
        self.upload_count = 0
        # Per-upload-attempt share outcomes consumed by files_upload_v2.
        # True = Slack shares the file; False = silent drop (ok but no share).
        # Empty list defaults every attempt to a successful share.
        self.share_outcomes: list[bool] = []
        self.files_info_calls: list[dict] = []
        self.files_delete_calls: list[dict] = []
        self.files_list_calls: list[dict] = []
        self.files_list_pages: list[dict] = []
        self._shares_by_file: dict[str, dict] = {}

    def chat_postMessage(self, **kwargs):
        self.last_kwargs = kwargs
        return {"ts": "123.456"}

    def conversations_history(self, **kwargs):
        self.history_calls.append(kwargs)
        return self.history_pages.pop(0)

    def conversations_replies(self, **kwargs):
        self.reply_calls.append(kwargs)
        return self.reply_pages.pop(0)

    def conversations_open(self, **kwargs):
        self.open_calls.append(kwargs)
        return self.open_response

    def users_list(self, **kwargs):
        self.users_calls.append(kwargs)
        return self.users_pages.pop(0)

    def users_info(self, **kwargs):
        self.users_calls.append(kwargs)
        return self.user_info_response or {"user": {}}

    def users_profile_get(self, **kwargs):
        self.user_profile_calls.append(kwargs)
        return self.user_profile_response or {"profile": {}}

    def conversations_list(self, **kwargs):
        self.list_calls.append(kwargs)
        return self.list_pages.pop(0)

    def users_conversations(self, **kwargs):
        self.user_conversations_calls.append(kwargs)
        return self.user_conversations_pages.pop(0)

    def files_upload_v2(self, **kwargs):
        self.last_kwargs = kwargs
        if self.upload_exception is not None:
            raise self.upload_exception
        self.upload_count += 1
        file_id = f"F{self.upload_count}"
        lands = self.share_outcomes.pop(0) if self.share_outcomes else True
        if lands:
            entry: dict = {"ts": "1.1", "channel_name": "paradigm-pulse"}
            if kwargs.get("thread_ts"):
                entry["thread_ts"] = kwargs["thread_ts"]
            self._shares_by_file[file_id] = {"public": {kwargs.get("channel", "C123"): [entry]}}
        else:
            self._shares_by_file[file_id] = {}
        return {
            "file": {
                "id": file_id,
                "name": kwargs.get("filename", "upload.png"),
                "permalink": f"https://slack.example/files/{file_id}",
                "url_private": f"https://files.example/{file_id}",
            }
        }

    def files_info(self, **kwargs):
        self.files_info_calls.append(kwargs)
        file_id = kwargs.get("file")
        return {"file": {"id": file_id, "shares": self._shares_by_file.get(file_id, {})}}

    def files_delete(self, **kwargs):
        self.files_delete_calls.append(kwargs)
        return {"ok": True}

    def files_list(self, **kwargs):
        self.files_list_calls.append(kwargs)
        return self.files_list_pages.pop(0)

    def api_call(self, method: str, *, params: dict):
        self.api_calls.append((method, params))
        return {"ok": True, "messages": {"matches": []}}


def _make_client() -> tuple[SlackClient, _FakeWebClient]:
    client = SlackClient.__new__(SlackClient)
    fake_web_client = _FakeWebClient()
    client._client = fake_web_client
    client._search_client = fake_web_client
    client._user_cache = {}
    client._ratelimit_deadlines = {}
    client._resolve_channel = lambda channel: "C123"  # type: ignore[method-assign]
    client._format_requester_attribution = lambda: ""  # type: ignore[method-assign]
    client.list_bot_channels = lambda **_: [{"id": "C123", "name": "paradigm-pulse"}]  # type: ignore[method-assign]
    return client, fake_web_client


def _make_slack_error(
    *, error: str, status_code: int, message: str = "Slack request failed"
) -> SlackApiError:
    return SlackApiError(
        message=message,
        response=_FakeSlackResponse(error=error, status_code=status_code),
    )


def test_send_message_forwards_unfurl_flags() -> None:
    client, fake_web_client = _make_client()

    client.send_message(
        "paradigm-pulse",
        "hello",
        unfurl_links=False,
        unfurl_media=False,
    )

    assert fake_web_client.last_kwargs is not None
    assert fake_web_client.last_kwargs["unfurl_links"] is False
    assert fake_web_client.last_kwargs["unfurl_media"] is False


def test_send_message_omits_unfurl_flags_by_default() -> None:
    client, fake_web_client = _make_client()

    client.send_message("paradigm-pulse", "hello")

    assert fake_web_client.last_kwargs is not None
    assert "unfurl_links" not in fake_web_client.last_kwargs
    assert "unfurl_media" not in fake_web_client.last_kwargs


def test_send_message_normalizes_escaped_line_breaks() -> None:
    client, fake_web_client = _make_client()

    client.send_message("paradigm-pulse", "*Title*\\n- one\\r\\n- two", no_attribution=True)

    assert fake_web_client.last_kwargs is not None
    assert fake_web_client.last_kwargs["text"] == "*Title*\n- one\n- two"


def test_send_message_opens_dm_for_user_id_destination() -> None:
    client, fake_web_client = _make_client()

    result = client.send_message("<@U123ABC>", "hello", no_attribution=True)

    assert fake_web_client.open_calls == [{"users": "U123ABC"}]
    assert fake_web_client.last_kwargs is not None
    assert fake_web_client.last_kwargs["channel"] == "D123"
    assert fake_web_client.last_kwargs["text"] == "hello"
    assert result["channel"] == "D123"
    assert result["permalink"] == "https://slack.com/archives/D123/p123456"


def test_send_dm_opens_dm_and_posts_message() -> None:
    client, fake_web_client = _make_client()

    client.send_dm("U234ABC", "hello", no_attribution=True, unfurl_links=False)

    assert fake_web_client.open_calls == [{"users": "U234ABC"}]
    assert fake_web_client.last_kwargs is not None
    assert fake_web_client.last_kwargs["channel"] == "D123"
    assert fake_web_client.last_kwargs["unfurl_links"] is False


def _restore_real_resolve_channel(client: SlackClient) -> None:
    client._resolve_channel = SlackClient._resolve_channel.__get__(client)  # type: ignore[method-assign]


def test_resolve_channel_opens_dm_for_user_id() -> None:
    client, fake_web_client = _make_client()
    _restore_real_resolve_channel(client)

    assert client._resolve_channel("<@U123ABC>") == "D123"
    assert fake_web_client.open_calls == [{"users": "U123ABC"}]


def test_resolve_channel_opens_dm_for_at_username() -> None:
    client, fake_web_client = _make_client()
    _restore_real_resolve_channel(client)
    client._get_user_cache = lambda: {"U123ABC": "georgios"}  # type: ignore[method-assign]

    assert client._resolve_channel("@georgios") == "D123"
    assert fake_web_client.open_calls == [{"users": "U123ABC"}]


def test_resolve_channel_rejects_unknown_at_username() -> None:
    client, _ = _make_client()
    _restore_real_resolve_channel(client)
    client._get_user_cache = lambda: {"U123ABC": "georgios"}  # type: ignore[method-assign]

    with pytest.raises(RuntimeError, match="not found in workspace"):
        client._resolve_channel("@nobody")


def test_resolve_channel_still_resolves_channel_names() -> None:
    client, fake_web_client = _make_client()
    _restore_real_resolve_channel(client)

    assert client._resolve_channel("paradigm-pulse") == "C123"
    assert client._resolve_channel("C456DEF") == "C456DEF"
    assert fake_web_client.open_calls == []


def test_retry_on_ratelimit_honors_retry_after(monkeypatch: pytest.MonkeyPatch) -> None:
    client, _ = _make_client()
    now = {"value": 100.0}
    sleeps: list[float] = []

    monkeypatch.setattr("slack.client.time.time", lambda: now["value"])

    def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)
        now["value"] += seconds

    monkeypatch.setattr("slack.client.time.sleep", fake_sleep)

    attempts = {"count": 0}

    def flaky_call() -> str:
        attempts["count"] += 1
        if attempts["count"] == 1:
            raise SlackApiError(
                message="rate limited",
                response=_FakeSlackResponse(headers={"Retry-After": "7"}),
            )
        return "ok"

    assert (
        client._retry_on_ratelimit(
            flaky_call,
            method_key="conversations.history",
            max_retry_sleep_s=10,
        )
        == "ok"
    )
    assert attempts["count"] == 2
    assert sleeps == [7.25]


def test_retry_on_ratelimit_fails_fast_by_default(monkeypatch: pytest.MonkeyPatch) -> None:
    client, _ = _make_client()
    sleeps: list[float] = []
    monkeypatch.setattr("slack.client.time.sleep", lambda seconds: sleeps.append(seconds))

    def rate_limited_call() -> str:
        raise SlackApiError(
            message="rate limited",
            response=_FakeSlackResponse(headers={"Retry-After": "30"}),
        )

    with pytest.raises(SlackRateLimitError) as excinfo:
        client._retry_on_ratelimit(rate_limited_call, method_key="conversations.history")

    payload = json.loads(str(excinfo.value))
    assert payload["error"] == "slack_rate_limited"
    assert payload["retry_after_seconds"] == 30.25
    assert sleeps == []


def test_get_channel_history_page_paginates_with_date_window() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {"U1": "alice", "U2": "bob"}  # type: ignore[method-assign]
    fake_web_client.history_pages = [
        {
            "messages": [
                {"user": "U1", "text": "first", "ts": "200.000000"},
                {
                    "user": "U2",
                    "text": "hi <@U1>",
                    "ts": "190.000000",
                    "thread_ts": "190.000000",
                    "reply_count": 1,
                },
            ],
            "response_metadata": {"next_cursor": "cursor-2"},
        },
        {
            "messages": [
                {"user": "U1", "text": "third", "ts": "180.000000"},
            ],
            "response_metadata": {"next_cursor": ""},
        },
    ]

    result = client.get_channel_history_page(
        "paradigm-pulse",
        limit=3,
        oldest="2026-01-01",
        latest="2026-01-02",
        inclusive=True,
    )

    assert len(fake_web_client.history_calls) == 2
    assert fake_web_client.history_calls[0]["oldest"] == client._normalize_ts("2026-01-01")
    assert fake_web_client.history_calls[0]["latest"] == client._normalize_ts("2026-01-02")
    assert fake_web_client.history_calls[0]["inclusive"] is True
    assert fake_web_client.history_calls[1]["cursor"] == "cursor-2"
    assert result["count"] == 3
    assert result["has_more"] is False
    assert result["messages"][1]["text"] == "hi @alice"


def test_get_channel_history_page_surfaces_structured_auth_failure() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {}  # type: ignore[method-assign]

    def fail_history(**kwargs):
        raise _make_slack_error(error="invalid_auth", status_code=401, message="Unauthorized")

    fake_web_client.conversations_history = fail_history  # type: ignore[method-assign]

    with pytest.raises(SlackAuthError) as excinfo:
        client.get_channel_history_page("paradigm-pulse")

    payload = json.loads(str(excinfo.value))
    assert payload == {
        "access_path": "bot_token",
        "error": "slack_auth_failed",
        "error_code": "invalid_auth",
        "message": "Slack authentication failed for conversations.history via bot_token",
        "requested_channel": "paradigm-pulse",
        "resolved_channel": "C123",
        "slack_method": "conversations.history",
        "status_code": 401,
    }


def test_get_user_profile_reads_labeled_custom_fields() -> None:
    client, fake_web_client = _make_client()
    fake_web_client.user_info_response = {
        "user": {
            "id": "U123",
            "name": "test-user",
            "real_name": "Test User",
            "tz": "Europe/London",
            "tz_label": "British Summer Time",
            "is_bot": False,
            "deleted": False,
        }
    }
    fake_web_client.user_profile_response = {
        "profile": {
            "display_name": "test-user",
            "email": "test.user@example.com",
            "fields": {
                "Xf123": {
                    "label": "Affiliations",
                    "value": "GitHub: test-user",
                    "alt": "",
                }
            },
        }
    }

    profile = client.get_user_profile("U123")

    assert fake_web_client.users_calls == [{"user": "U123"}]
    assert fake_web_client.user_profile_calls == [{"user": "U123", "include_labels": True}]
    assert profile["custom_fields"] == {"Affiliations": "GitHub: test-user"}
    assert profile["raw_custom_fields"] == {
        "Xf123": {"label": "Affiliations", "value": "GitHub: test-user", "alt": ""}
    }


def test_get_channel_history_page_preserves_non_auth_error_shape() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {}  # type: ignore[method-assign]

    def fail_history(**kwargs):
        raise _make_slack_error(error="channel_not_found", status_code=404)

    fake_web_client.conversations_history = fail_history  # type: ignore[method-assign]

    with pytest.raises(RuntimeError, match="Slack API error: channel_not_found"):
        client.get_channel_history_page("paradigm-pulse")


def test_get_channel_history_proxy_calls_centaur_api(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import urllib.parse
    import urllib.request

    client, _ = _make_client()
    request_info: dict[str, str | None] = {}

    def fake_urlopen(req, *args, **kwargs):
        request_info["url"] = req.full_url
        request_info["authorization"] = req.get_header("Authorization")
        body = json.dumps(
            {
                "ok": True,
                "messages": [{"type": "message", "ts": "1700000000.000001"}],
                "has_more": False,
            }
        ).encode()
        return _FakeHTTPResponse(body, "application/json")

    monkeypatch.setenv("CENTAUR_API_URL", "http://api.internal:8080")
    monkeypatch.setenv("CENTAUR_API_BEARER_TOKEN", "test-jwt")
    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    result = client.get_channel_history_proxy(
        "<#C123456789|general>",
        cursor="next",
        include_all_metadata=True,
        inclusive=False,
        latest="1700000000.000002",
        limit=999,
        oldest=0,
    )

    assert result["ok"] is True
    assert request_info["authorization"] == "Bearer test-jwt"
    parsed = urllib.parse.urlparse(request_info["url"])
    assert parsed.scheme == "http"
    assert parsed.netloc == "api.internal:8080"
    assert parsed.path == "/api/slack/channels/C123456789/history"
    query = urllib.parse.parse_qs(parsed.query)
    assert query == {
        "cursor": ["next"],
        "include_all_metadata": ["true"],
        "inclusive": ["false"],
        "latest": ["1700000000.000002"],
        "limit": ["999"],
        "oldest": ["0.000000"],
    }


def test_get_channel_history_proxy_validates_inputs() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="channel_id"):
        client.get_channel_history_proxy("general")

    with pytest.raises(ValueError, match="between 1 and 999"):
        client.get_channel_history_proxy("C123456789", limit=1000)


def test_list_channels_proxy_calls_centaur_api() -> None:
    client, _ = _make_client()

    def fake_get_json(path, params):
        assert path == "/api/slack/channels"
        assert params == {}
        return {
            "ok": True,
            "channels": [
                {
                    "id": "C222222222",
                    "name": "random",
                    "purpose": "",
                    "topic": "Chat",
                    "member_count": 3,
                    "is_private": False,
                    "is_member": True,
                    "can_upload": True,
                    "can_download": False,
                    "can_read_history": False,
                },
                {
                    "id": "C111111111",
                    "name": "general",
                    "purpose": "Company",
                    "topic": "",
                    "member_count": 10,
                    "is_private": False,
                    "is_member": True,
                    "can_upload": False,
                    "can_download": True,
                    "can_read_history": True,
                },
            ],
        }

    client._centaur_api_get_json = fake_get_json  # type: ignore[method-assign]

    assert [channel["id"] for channel in client.list_channels_proxy()] == [
        "C111111111",
        "C222222222",
    ]
    assert [channel["id"] for channel in client.list_channels_proxy(history_only=True)] == [
        "C111111111"
    ]


def test_list_files_proxy_calls_centaur_api() -> None:
    client, _ = _make_client()

    def fake_get_json(path, params):
        assert path == "/api/slack/files"
        assert params == {
            "channel_id": "C123456789",
            "limit": 20,
            "page": 3,
        }
        return {
            "ok": True,
            "files": [{"id": "F123456789", "name": "report.pdf"}],
            "page": 3,
            "has_more": True,
        }

    client._centaur_api_get_json = fake_get_json  # type: ignore[method-assign]

    result = client.list_files_proxy("<#C123456789|general>", page=3, limit=20)

    assert result["files"] == [{"id": "F123456789", "name": "report.pdf"}]
    assert result["has_more"] is True


def test_list_files_proxy_validates_inputs() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="channel_id"):
        client.list_files_proxy("general")

    with pytest.raises(ValueError, match="between 1 and 200"):
        client.list_files_proxy("C123456789", limit=201)

    with pytest.raises(ValueError, match="page"):
        client.list_files_proxy("C123456789", page=0)


def test_get_thread_replies_proxy_calls_centaur_api(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import urllib.parse
    import urllib.request

    client, _ = _make_client()
    request_info: dict[str, str | None] = {}

    def fake_urlopen(req, *args, **kwargs):
        request_info["url"] = req.full_url
        request_info["authorization"] = req.get_header("Authorization")
        body = json.dumps(
            {
                "ok": True,
                "messages": [{"type": "message", "ts": "1700000000.000001"}],
                "has_more": False,
            }
        ).encode()
        return _FakeHTTPResponse(body, "application/json")

    monkeypatch.setenv("CENTAUR_API_URL", "http://api.internal:8080")
    monkeypatch.setenv("CENTAUR_API_BEARER_TOKEN", "test-jwt")
    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    result = client.get_thread_replies_proxy(
        "<#C123456789|general>",
        "1700000000.000001",
        cursor="next",
        inclusive=False,
        latest="1700000000.000002",
        limit=999,
        oldest=0,
    )

    assert result["ok"] is True
    assert request_info["authorization"] == "Bearer test-jwt"
    parsed = urllib.parse.urlparse(request_info["url"])
    assert parsed.scheme == "http"
    assert parsed.netloc == "api.internal:8080"
    assert parsed.path == "/api/slack/channels/C123456789/threads/1700000000.000001/replies"
    query = urllib.parse.parse_qs(parsed.query)
    assert query == {
        "cursor": ["next"],
        "inclusive": ["false"],
        "latest": ["1700000000.000002"],
        "limit": ["999"],
        "oldest": ["0.000000"],
    }


def test_get_thread_replies_proxy_validates_inputs() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="channel_id"):
        client.get_thread_replies_proxy("general", "1700000000.000001")

    with pytest.raises(ValueError, match="thread_ts"):
        client.get_thread_replies_proxy("C123456789", "")

    with pytest.raises(ValueError, match="between 1 and 999"):
        client.get_thread_replies_proxy("C123456789", "1700000000.000001", limit=1000)


def test_upload_file_proxy_posts_file_bytes_to_centaur_api(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import urllib.parse
    import urllib.request

    client, _ = _make_client()
    request_info: dict[str, object] = {}

    def fake_urlopen(req, *args, **kwargs):
        request_info["url"] = req.full_url
        request_info["headers"] = {key.lower(): value for key, value in req.header_items()}
        request_info["data"] = req.data
        body = json.dumps(
            {
                "ok": True,
                "file_id": "F123456789",
                "channel_id": "C123456789",
                "file": {"id": "F123456789"},
            }
        ).encode()
        return _FakeHTTPResponse(body, "application/json")

    monkeypatch.setenv("CENTAUR_API_URL", "http://api.internal:8080")
    monkeypatch.setenv("CENTAUR_API_BEARER_TOKEN", "test-jwt")
    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    result = client.upload_file_proxy(
        channel_id="C123456789",
        content_base64=base64.b64encode(b"hello").decode(),
        filename="hello.txt",
        thread_ts="1700000000.000001",
        title="Hello",
        initial_comment="uploaded",
        content_type="text/plain",
        alt_txt="hello file",
        snippet_type="text",
    )

    assert result["file_id"] == "F123456789"
    assert request_info["data"] == b"hello"
    headers = request_info["headers"]
    assert isinstance(headers, dict)
    assert headers["authorization"] == "Bearer test-jwt"
    assert headers["content-type"] == "text/plain"
    parsed = urllib.parse.urlparse(request_info["url"])
    assert parsed.path == "/api/slack/files/upload"
    assert urllib.parse.parse_qs(parsed.query) == {
        "channel_id": ["C123456789"],
        "filename": ["hello.txt"],
        "thread_ts": ["1700000000.000001"],
        "title": ["Hello"],
        "initial_comment": ["uploaded"],
        "content_type": ["text/plain"],
        "alt_txt": ["hello file"],
        "snippet_type": ["text"],
    }


def test_download_file_proxy_returns_base64_file(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import urllib.parse
    import urllib.request

    client, _ = _make_client()
    request_info: dict[str, str | None] = {}

    def fake_urlopen(req, *args, **kwargs):
        request_info["url"] = req.full_url
        request_info["authorization"] = req.get_header("Authorization")
        return _FakeHTTPResponse(
            b"%PDF",
            "application/pdf",
            {"Content-Disposition": 'attachment; filename="report.pdf"'},
        )

    monkeypatch.setenv("CENTAUR_API_URL", "http://api.internal:8080")
    monkeypatch.setenv("CENTAUR_API_BEARER_TOKEN", "test-jwt")
    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    result = client.download_file_proxy(file_id="F123456789", channel_id="C123456789")

    assert result == {
        "file_id": "F123456789",
        "channel_id": "C123456789",
        "filename": "report.pdf",
        "content_type": "application/pdf",
        "size_bytes": 4,
        "content_base64": base64.b64encode(b"%PDF").decode(),
    }
    assert request_info["authorization"] == "Bearer test-jwt"
    parsed = urllib.parse.urlparse(request_info["url"])
    assert parsed.path == "/api/slack/files/F123456789/download"
    assert urllib.parse.parse_qs(parsed.query) == {"channel_id": ["C123456789"]}


def test_file_info_proxy_calls_centaur_api(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import urllib.parse
    import urllib.request

    client, _ = _make_client()
    request_info: dict[str, str | None] = {}

    def fake_urlopen(req, *args, **kwargs):
        request_info["url"] = req.full_url
        request_info["authorization"] = req.get_header("Authorization")
        body = json.dumps(
            {
                "ok": True,
                "file_id": "F123456789",
                "channel_id": "C123456789",
                "file": {"id": "F123456789", "name": "report.pdf"},
            }
        ).encode()
        return _FakeHTTPResponse(body, "application/json")

    monkeypatch.setenv("CENTAUR_API_URL", "http://api.internal:8080")
    monkeypatch.setenv("CENTAUR_API_BEARER_TOKEN", "test-jwt")
    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    result = client.file_info_proxy(file_id="F123456789", channel_id="<#C123456789|general>")

    assert result["file"] == {"id": "F123456789", "name": "report.pdf"}
    assert request_info["authorization"] == "Bearer test-jwt"
    parsed = urllib.parse.urlparse(request_info["url"])
    assert parsed.path == "/api/slack/files/F123456789/info"
    assert urllib.parse.parse_qs(parsed.query) == {"channel_id": ["C123456789"]}


def test_get_channel_members_proxy_paginates_and_resolves_names() -> None:
    client, _ = _make_client()
    client._get_user_cache = lambda: {"U111111111": "alice", "U222222222": "bob"}  # type: ignore[method-assign]
    calls: list[tuple[str, dict]] = []

    def fake_get_json(path, params):
        calls.append((path, params))
        if params["cursor"] is None:
            return {
                "ok": True,
                "members": ["U111111111"],
                "response_metadata": {"next_cursor": "next"},
            }
        return {
            "ok": True,
            "members": ["U222222222"],
            "response_metadata": {"next_cursor": ""},
        }

    client._centaur_api_get_json = fake_get_json  # type: ignore[method-assign]

    result = client.get_channel_members_proxy("<#C123456789|general>", limit=10)

    assert calls == [
        ("/api/slack/channels/C123456789/members", {"limit": 10, "cursor": None}),
        ("/api/slack/channels/C123456789/members", {"limit": 9, "cursor": "next"}),
    ]
    assert result == [
        {"id": "U111111111", "name": "alice"},
        {"id": "U222222222", "name": "bob"},
    ]


def test_file_proxy_methods_validate_inputs() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="filename"):
        client.upload_file_proxy(
            channel_id="C123456789",
            content_base64=base64.b64encode(b"hello").decode(),
            filename=" ",
        )
    with pytest.raises(ValueError, match="valid base64"):
        client.upload_file_proxy(
            channel_id="C123456789",
            content_base64="not base64",
            filename="hello.txt",
        )
    with pytest.raises(ValueError, match="file_id"):
        client.download_file_proxy(file_id="bad", channel_id="C123456789")
    with pytest.raises(ValueError, match="file_id"):
        client.file_info_proxy(file_id="bad", channel_id="C123456789")
    with pytest.raises(ValueError, match="channel_id"):
        client.get_channel_members_proxy(channel_id="general")


def test_search_files_uses_proxy_with_user_cache(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client, _ = _make_client()
    monkeypatch.setenv("CENTAUR_SANDBOX_API_SERVER_ENABLED", "true")
    client._get_user_cache = lambda: {"U123456789": "alice"}  # type: ignore[method-assign]

    def fake_list_files_proxy(**kwargs):
        assert kwargs == {"channel_id": "C123456789", "limit": 200, "page": 1}
        return {
            "ok": True,
            "has_more": False,
            "files": [
                {
                    "id": "F123456789",
                    "name": "quarterly-report.pdf",
                    "title": "Q4 Report",
                    "filetype": "pdf",
                    "size": 1234,
                    "user": "U123456789",
                    "channels": ["C123456789"],
                    "permalink": "https://slack.example/files/F123456789",
                    "url_private": "https://files.example/F123456789",
                    "created": 1700000000,
                },
                {
                    "id": "F987654321",
                    "name": "diagram.png",
                    "title": "Architecture Diagram",
                    "filetype": "png",
                    "size": 5678,
                    "user": "U987654321",
                    "channels": ["C123456789"],
                    "permalink": "https://slack.example/files/F987654321",
                    "url_private": "https://files.example/F987654321",
                    "created": 1700000001,
                },
            ],
        }

    client.list_files_proxy = fake_list_files_proxy  # type: ignore[method-assign]

    results = client.search_files("C123456789", "report", max_results=20)

    assert results == [
        {
            "id": "F123456789",
            "name": "quarterly-report.pdf",
            "title": "Q4 Report",
            "filetype": "pdf",
            "size": 1234,
            "user": "alice",
            "channels": ["C123456789"],
            "permalink": "https://slack.example/files/F123456789",
            "url_private": "https://files.example/F123456789",
            "created": 1700000000,
        }
    ]


def test_search_files_raises_when_api_proxy_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client, _ = _make_client()
    monkeypatch.setenv("CENTAUR_SANDBOX_API_SERVER_ENABLED", "false")

    with pytest.raises(RuntimeError, match="proxy requires"):
        client.search_files("C123456789", "report", max_results=10)


def test_search_files_paginates_proxy_until_enough_matches() -> None:
    client, _ = _make_client()
    client._get_user_cache = lambda: {"U123456789": "alice"}  # type: ignore[method-assign]
    calls: list[dict] = []

    def fake_list_files_proxy(**kwargs):
        calls.append(kwargs)
        if kwargs["page"] == 1:
            return {
                "ok": True,
                "has_more": True,
                "files": [
                    {
                        "id": "F000000001",
                        "name": "diagram.png",
                        "title": "Diagram",
                        "filetype": "png",
                        "size": 1234,
                        "user": "U123456789",
                        "channels": ["C123456789"],
                        "permalink": "https://slack.example/files/F000000001",
                        "url_private": "https://files.example/F000000001",
                        "created": 1700000000,
                    }
                ],
            }
        return {
            "ok": True,
            "has_more": False,
            "files": [
                {
                    "id": "F123456789",
                    "name": "quarterly-report.pdf",
                    "title": "Q4 Report",
                    "filetype": "pdf",
                    "size": 1234,
                    "user": "U123456789",
                    "channels": ["C123456789"],
                    "permalink": "https://slack.example/files/F123456789",
                    "url_private": "https://files.example/F123456789",
                    "created": 1700000001,
                }
            ],
        }

    client.list_files_proxy = fake_list_files_proxy  # type: ignore[method-assign]

    results = client.search_files("C111111111", "report", max_results=10)

    assert calls == [
        {"channel_id": "C111111111", "limit": 200, "page": 1},
        {"channel_id": "C111111111", "limit": 200, "page": 2},
    ]
    assert [result["id"] for result in results] == ["F123456789"]


def test_search_files_direct_uses_direct_files_list_when_api_proxy_enabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client, fake_web_client = _make_client()
    monkeypatch.setenv("CENTAUR_SANDBOX_API_SERVER_ENABLED", "true")
    client._get_user_cache = lambda: {"U123456789": "alice"}  # type: ignore[method-assign]
    client.list_files_proxy = pytest.fail  # type: ignore[method-assign]
    fake_web_client.files_list_pages = [
        {
            "paging": {"page": 1, "pages": 1},
            "files": [
                {
                    "id": "F123456789",
                    "name": "quarterly-report.pdf",
                    "title": "Q4 Report",
                    "filetype": "pdf",
                    "size": 1234,
                    "user": "U123456789",
                    "channels": ["C123456789"],
                    "permalink": "https://slack.example/files/F123456789",
                    "url_private": "https://files.example/F123456789",
                    "created": 1700000000,
                }
            ]
        }
    ]

    results = client.search_files_direct("report", max_results=10)

    assert fake_web_client.files_list_calls == [{"count": 200, "page": 1}]
    assert results[0]["user"] == "alice"


def test_search_messages_with_channel_ids_scans_proxy_history_without_listing() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {"UGZCSQTPE": "matt", "U1": "alice"}  # type: ignore[method-assign]
    history_by_channel = {
        "C05HUE4KLF2": {
            "messages": [
                {"user": "UGZCSQTPE", "text": "Matt note about inference", "ts": "300.000000"},
                {"user": "U1", "text": "unrelated", "ts": "299.000000"},
            ]
        },
        "C042WDDP89Y": {
            "messages": [{"user": "U1", "text": "Matt mentioned here", "ts": "200.000000"}]
        },
        "C0A174PPJDS": {
            "messages": [{"user": "UGZCSQTPE", "text": "nothing relevant", "ts": "100.000000"}]
        },
    }
    proxy_calls: list[dict] = []

    def history_proxy(channel_id: str, **kwargs):
        proxy_calls.append({"channel_id": channel_id, **kwargs})
        return history_by_channel[channel_id]

    client.get_channel_history_proxy = history_proxy  # type: ignore[method-assign]

    results = client.search_messages(
        "Matt",
        max_results=10,
        channels=["C05HUE4KLF2", "C042WDDP89Y", "C0A174PPJDS"],
        messages_per_channel=25,
    )

    assert fake_web_client.api_calls == []
    assert fake_web_client.list_calls == []
    assert fake_web_client.history_calls == []
    assert sorted(call["channel_id"] for call in proxy_calls) == sorted(
        [
            "C05HUE4KLF2",
            "C042WDDP89Y",
            "C0A174PPJDS",
        ]
    )
    assert sorted(call["limit"] for call in proxy_calls) == [25, 25, 25]
    assert sorted(item["channel_id"] for item in results) == ["C042WDDP89Y", "C05HUE4KLF2"]


def test_search_messages_parses_channel_and_user_modifiers_locally() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {"UGZCSQTPE": "matt", "U1": "alice"}  # type: ignore[method-assign]
    proxy_calls: list[dict] = []

    def history_proxy(channel_id: str, **kwargs):
        proxy_calls.append({"channel_id": channel_id, **kwargs})
        return {
            "messages": [
                {"user": "UGZCSQTPE", "text": "Scott Wu on inference", "ts": "300.000000"},
                {"user": "U1", "text": "also about inference", "ts": "301.000000"},
            ]
        }

    client.get_channel_history_proxy = history_proxy  # type: ignore[method-assign]

    results = client.search_messages(
        "from:<@UGZCSQTPE> in:<#C042WDDP89Y>",
        max_results=5,
        messages_per_channel=25,
    )

    assert fake_web_client.api_calls == []
    assert fake_web_client.list_calls == []
    assert fake_web_client.history_calls == []
    assert proxy_calls == [{"channel_id": "C042WDDP89Y", "limit": 25}]
    assert len(results) == 1
    assert results[0]["user_id"] == "UGZCSQTPE"


def test_list_channels_returns_cache_when_slack_rate_limited() -> None:
    client, fake_web_client = _make_client()
    cached_channels = [{"id": "C123", "name": "cached", "is_private": False}]
    client._load_channel_cache = lambda: (cached_channels, 100.0)  # type: ignore[method-assign]

    def rate_limited_list(**kwargs):
        raise SlackApiError(
            message="rate limited",
            response=_FakeSlackResponse(headers={"Retry-After": "30"}),
        )

    fake_web_client.conversations_list = rate_limited_list  # type: ignore[method-assign]

    assert client.list_channels(limit=10) == cached_channels


def test_list_bot_channels_uses_users_conversations() -> None:
    client, fake_web_client = _make_client()
    saved: list[list[dict]] = []
    client._save_channel_cache = lambda result: saved.append(result)  # type: ignore[method-assign]

    fake_web_client.user_conversations_pages = [
        {
            "channels": [
                {"id": "C1", "name": "zeta", "is_private": False, "num_members": 3},
                {"id": "C2", "name": "alpha", "is_private": True, "num_members": 5},
            ],
            "response_metadata": {"next_cursor": ""},
        }
    ]

    # Call the real implementation (the fixture stubs list_bot_channels out).
    result = SlackClient.list_bot_channels(client, force_refresh=True)

    # Membership is scoped via users.conversations rather than a whole-workspace
    # conversations.list scan + client-side is_member filter.
    assert len(fake_web_client.user_conversations_calls) == 1
    call = fake_web_client.user_conversations_calls[0]
    assert call["types"] == "public_channel,private_channel"
    assert call["exclude_archived"] is True
    assert fake_web_client.list_calls == []

    # Every conversation returned by the API is kept (membership is implied),
    # sorted by name, with the expected fields preserved.
    assert [c["id"] for c in result] == ["C2", "C1"]
    assert [c["name"] for c in result] == ["alpha", "zeta"]
    assert result[0]["is_private"] is True
    assert result[1]["member_count"] == 3


def test_get_thread_replies_page_uses_bounded_default() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {}  # type: ignore[method-assign]
    fake_web_client.reply_pages = [
        {
            "messages": [{"user": "U1", "text": "root", "ts": "100.000000"}],
            "response_metadata": {"next_cursor": ""},
        }
    ]

    result = client.get_thread_replies_page("paradigm-pulse", "100.000000")

    assert fake_web_client.reply_calls[0]["limit"] == 50
    assert result["effective_limit"] == 50
    assert result["continuation_available"] is False


def test_dump_channel_with_threads_limits_thread_expansion() -> None:
    client, fake_web_client = _make_client()
    client._get_user_cache = lambda: {}  # type: ignore[method-assign]
    fake_web_client.history_pages = [
        {
            "messages": [
                {"user": "U1", "text": "root 1", "ts": "101.000000", "reply_count": 2},
                {"user": "U2", "text": "root 2", "ts": "102.000000", "reply_count": 2},
            ],
            "response_metadata": {"next_cursor": ""},
        }
    ]
    fake_web_client.reply_pages = [
        {
            "messages": [
                {"user": "U1", "text": "root 1", "ts": "101.000000"},
                {"user": "U2", "text": "reply", "ts": "101.000001"},
            ],
            "response_metadata": {"next_cursor": ""},
        }
    ]

    result = client.dump_channel_with_threads(
        "paradigm-pulse",
        max_threads=1,
        replies_limit=500,
    )

    assert fake_web_client.history_calls[0]["limit"] == 100
    assert fake_web_client.reply_calls[0]["limit"] == 200
    assert result["stats"]["threads_expanded"] == 1
    assert result["stats"]["threads_skipped_by_limit"] == 1
    assert result["continuation_available"] is True
    assert result["limits"] == {
        "message_limit": 100,
        "reply_limit": 200,
        "thread_limit": 1,
    }


def test_upload_file_surfaces_structured_auth_failure() -> None:
    client, fake_web_client = _make_client()
    fake_web_client.upload_exception = _make_slack_error(
        error="not_authed",
        status_code=401,
        message="Unauthorized",
    )

    with pytest.raises(SlackAuthError) as excinfo:
        client.upload_file(
            "paradigm-pulse",
            thread_ts="1780035646.228899",
            content_base64="dGVzdA==",
            filename="chart.png",
        )

    payload = json.loads(str(excinfo.value))
    assert payload == {
        "access_path": "file_upload",
        "error": "slack_auth_failed",
        "error_code": "not_authed",
        "message": "Slack authentication failed for files.upload_v2 via file_upload",
        "requested_channel": "paradigm-pulse",
        "resolved_channel": "C123",
        "slack_method": "files.upload_v2",
        "status_code": 401,
    }


def test_upload_file_accepts_channel_id_alias_and_returns_preview() -> None:
    client, fake_web_client = _make_client()

    result = client.upload_file(
        None,
        channel_id="paradigm-pulse",
        thread_ts="1780035646.228899",
        content_base64="YSxiCjEsMgo=",
        filename="data.csv",
    )

    assert fake_web_client.last_kwargs is not None
    assert fake_web_client.last_kwargs["channel"] == "C123"
    assert fake_web_client.last_kwargs["filename"] == "data.csv"
    assert fake_web_client.last_kwargs["file"] == b"a,b\n1,2\n"
    assert result["preview"] == {
        "size_bytes": 8,
        "mime_type": "text/csv",
        "csv_rows_sampled": 1,
        "csv_columns": 2,
    }


def test_upload_file_uses_explicit_destination() -> None:
    resolved_channels: list[str] = []
    client, fake_web_client = _make_client()

    def resolve_channel(channel: str) -> str:
        resolved_channels.append(channel)
        return channel

    client._resolve_channel = resolve_channel  # type: ignore[method-assign]

    client.upload_file(
        channel_id="C-explicit",
        thread_ts="201.000000",
        content_base64="dGVzdA==",
        filename="chart.png",
    )

    assert resolved_channels == ["C-explicit"]
    assert fake_web_client.last_kwargs is not None
    assert fake_web_client.last_kwargs["channel"] == "C-explicit"
    assert fake_web_client.last_kwargs["thread_ts"] == "201.000000"


def test_upload_file_requires_explicit_channel() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="channel is required"):
        client.upload_file(
            thread_ts="1780035646.228899",
            content_base64="dGVzdA==",
            filename="random_data.csv",
        )


def test_upload_file_requires_explicit_thread_ts() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="thread_ts is required"):
        client.upload_file(
            channel_id="C123",
            content_base64="dGVzdA==",
            filename="random_data.csv",
        )


def test_upload_file_uploads_once_and_returns_when_share_lands(monkeypatch) -> None:
    """The stripped path does a single upload and verifies via files.info; no
    retry, no orphan deletion."""
    import slack.client as slack_client_module

    monkeypatch.setattr(slack_client_module.time, "sleep", lambda *a, **k: None)

    client, fake_web_client = _make_client()

    result = client.upload_file(
        channel_id="C123",
        thread_ts="1780035646.228899",
        content_base64="dGVzdA==",
        filename="random.csv",
    )

    assert fake_web_client.upload_count == 1
    assert fake_web_client.files_delete_calls == []
    assert result["id"] == "F1"


def test_upload_file_returns_dropped_result_without_retry(monkeypatch) -> None:
    """A silent share drop is logged but not retried or deleted; the (phantom)
    result is returned so we can observe the raw rate."""
    import slack.client as slack_client_module

    monkeypatch.setattr(slack_client_module.time, "sleep", lambda *a, **k: None)

    client, fake_web_client = _make_client()
    fake_web_client.share_outcomes = [False]  # share never lands

    result = client.upload_file(
        channel_id="C123",
        thread_ts="1780035646.228899",
        content_base64="dGVzdA==",
        filename="random.csv",
    )

    assert fake_web_client.upload_count == 1
    assert fake_web_client.files_delete_calls == []
    assert result["id"] == "F1"


def test_upload_file_returns_result_when_verification_unavailable(monkeypatch) -> None:
    """When files.info cannot confirm the share (e.g. missing files:read scope),
    upload_file returns the result as-is."""
    import slack.client as slack_client_module

    monkeypatch.setattr(slack_client_module.time, "sleep", lambda *a, **k: None)

    client, fake_web_client = _make_client()

    def _boom(**_kwargs):
        raise _make_slack_error(error="missing_scope", status_code=403)

    fake_web_client.files_info = _boom  # type: ignore[method-assign]

    result = client.upload_file(
        channel_id="C123",
        thread_ts="1780035646.228899",
        content_base64="dGVzdA==",
        filename="random.csv",
    )

    assert fake_web_client.upload_count == 1
    assert fake_web_client.files_delete_calls == []
    assert result["id"] == "F1"


def test_upload_file_never_sends_alt_txt(monkeypatch) -> None:
    """alt_text is accepted for compatibility but never forwarded to Slack,
    because slack_sdk's files_upload_v2 mishandles alt_txt
    (slackapi/python-slack-sdk#1818)."""
    import slack.client as slack_client_module

    monkeypatch.setattr(slack_client_module.time, "sleep", lambda *a, **k: None)

    client, fake_web_client = _make_client()
    client.upload_file(
        channel_id="C123",
        thread_ts="1780035646.228899",
        content_base64="dGVzdA==",
        filename="chart.png",
        alt_text="a bar chart",
    )
    assert "alt_txt" not in fake_web_client.last_kwargs


def test_upload_file_rejects_local_path_argument() -> None:
    """upload_file must not accept a local path: it runs server-side, so a
    caller path would read the API host's filesystem."""
    client, _ = _make_client()

    with pytest.raises(TypeError):
        client.upload_file("paradigm-pulse", file_path="/tmp/missing-chart.png")


def test_upload_file_requires_a_content_source() -> None:
    client, _ = _make_client()

    with pytest.raises(ValueError, match="content_base64 is required"):
        client.upload_file("paradigm-pulse", thread_ts="1780035646.228899")


class _FakeHTTPResponse:
    """Minimal stand-in for urllib's HTTPResponse context manager."""

    def __init__(
        self, body: bytes, content_type: str, headers: dict[str, str] | None = None
    ) -> None:
        self._body = body
        self._content_type = content_type
        self._headers = headers or {}

    def __enter__(self) -> "_FakeHTTPResponse":
        return self

    def __exit__(self, *exc: object) -> bool:
        return False

    def read(self, _amt: int = -1) -> bytes:
        return self._body

    @property
    def headers(self) -> "email.message.Message":
        msg = email.message.Message()
        msg["Content-Type"] = self._content_type
        for key, value in self._headers.items():
            msg[key] = value
        return msg


def test_fetch_slack_file_rejects_non_files_host() -> None:
    client, _ = _make_client()
    client.token = "SLACK_BOT_TOKEN"

    with pytest.raises(ValueError, match=r"files\.slack\.com"):
        client._fetch_slack_file("https://slack.com/api/api.test?x=SLACK_BOT_TOKEN")

    with pytest.raises(ValueError, match=r"files\.slack\.com"):
        client._fetch_slack_file("http://files.slack.com/files-pri/T1-F1/report.pdf")


def test_fetch_slack_file_returns_file_metadata_and_bytes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import urllib.request

    client, _ = _make_client()
    client.token = "SLACK_BOT_TOKEN"

    def fake_urlopen(req, *args, **kwargs):
        if "files.slack.com" in req.full_url:
            assert req.get_header("Authorization") == "Bearer SLACK_BOT_TOKEN"
            return _FakeHTTPResponse(b"%PDF-1.4 report", "application/pdf")
        raise AssertionError(f"unexpected url {req.full_url}")

    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    filename, mime_type, body = client._fetch_slack_file(
        "https://files.slack.com/files-pri/T1-F1/report.pdf"
    )

    assert filename == "report.pdf"
    assert mime_type == "application/pdf"
    assert body == b"%PDF-1.4 report"


def test_native_search_uses_dedicated_search_client() -> None:
    client, fake_bot_client = _make_client()
    fake_search_client = _FakeWebClient()
    fake_search_client.api_call = lambda method, *, params: {  # type: ignore[method-assign]
        "ok": True,
        "messages": {
            "matches": [
                {
                    "user": "U1",
                    "text": "deploy <@U2>",
                    "ts": "200.000000",
                    "permalink": "https://slack.com/archives/C123/p200000000",
                    "channel": {"id": "C123", "name": "paradigm-pulse"},
                    "thread_ts": "200.000000",
                    "reply_count": 2,
                }
            ]
        },
    }
    client._search_client = fake_search_client
    client._get_user_cache = lambda: {"U1": "alice", "U2": "bob"}  # type: ignore[method-assign]

    result = client._search_messages_native("deploy", max_results=5)

    assert result == [
        {
            "channel": "paradigm-pulse",
            "channel_id": "C123",
            "user": "alice",
            "user_id": "U1",
            "text": "deploy @bob",
            "timestamp": "200.000000",
            "permalink": "https://slack.com/archives/C123/p200000000",
            "thread_ts": "200.000000",
            "reply_count": 2,
        }
    ]
    assert fake_bot_client.api_calls == []


def test_sync_channel_history_uses_watermark_lookback() -> None:
    client, _ = _make_client()
    captured: dict = {}

    def fake_get_channel_history_page(**kwargs):
        captured.update(kwargs)
        return {
            "channel": "paradigm-pulse",
            "channel_id": "C123",
            "messages": [{"timestamp": "3000100.000000"}],
            "count": 1,
            "has_more": False,
            "next_cursor": None,
            "window": {
                "oldest": kwargs["oldest"],
                "latest": kwargs["latest"],
                "inclusive": kwargs["inclusive"],
            },
            "order": "desc",
        }

    client.get_channel_history_page = fake_get_channel_history_page  # type: ignore[method-assign]

    result = client.sync_channel_history(
        "paradigm-pulse",
        state={"watermark": "3000000.000000"},
        lookback_days=30,
        limit=100,
    )

    assert captured["oldest"] == "408000.000000"
    assert captured["inclusive"] is True
    assert result["sync_state"]["cursor"] is None
    assert result["sync_state"]["watermark"] == "3000100.000000"


def test_list_users_paginates_and_skips_deleted_by_default() -> None:
    client, fake_web_client = _make_client()
    fake_web_client.users_pages = [
        {
            "members": [
                {
                    "id": "U1",
                    "name": "alice",
                    "real_name": "Alice Example",
                    "profile": {"display_name": "Alice"},
                },
                {
                    "id": "U2",
                    "name": "deleted",
                    "deleted": True,
                },
            ],
            "response_metadata": {"next_cursor": "cursor-2"},
        },
        {
            "members": [
                {
                    "id": "U3",
                    "name": "bob",
                    "real_name": "Bob Example",
                    "team_id": "T1",
                    "profile": {"display_name": "Bobby"},
                },
            ],
            "response_metadata": {"next_cursor": ""},
        },
    ]

    users = client.list_users(limit=10)

    assert [user["id"] for user in users] == ["U1", "U3"]
    assert users[0]["display_name"] == "Alice"
    assert users[1]["team_id"] == "T1"
    assert fake_web_client.users_calls == [
        {"limit": 10},
        {"limit": 9, "cursor": "cursor-2"},
    ]
