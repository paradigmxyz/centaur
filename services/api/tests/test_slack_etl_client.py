from __future__ import annotations

import json
from typing import Any

import pytest

from workflows.slack_sync_shared import SlackEtlAuthError, SlackEtlClient


class _FakeSlackResponse(dict):
    def __init__(
        self,
        *,
        error: str = "ratelimited",
        headers: dict[str, str] | None = None,
        status_code: int = 429,
    ) -> None:
        super().__init__(error=error)
        self.headers = headers or {}
        self.status_code = status_code


class _FakeSlackError(Exception):
    def __init__(
        self,
        *,
        error: str,
        status_code: int,
        headers: dict[str, str] | None = None,
    ) -> None:
        super().__init__(error)
        self.response = _FakeSlackResponse(
            error=error,
            status_code=status_code,
            headers=headers,
        )


class _FakeWebClient:
    def __init__(self) -> None:
        self.history_calls: list[dict[str, Any]] = []
        self.history_pages: list[dict[str, Any]] = []
        self.reply_calls: list[dict[str, Any]] = []
        self.reply_pages: list[dict[str, Any]] = []
        self.users_calls: list[dict[str, Any]] = []
        self.users_pages: list[dict[str, Any]] = []
        self.list_calls: list[dict[str, Any]] = []
        self.list_pages: list[dict[str, Any]] = []

    def conversations_history(self, **kwargs: Any) -> dict[str, Any]:
        self.history_calls.append(kwargs)
        return self.history_pages.pop(0)

    def conversations_replies(self, **kwargs: Any) -> dict[str, Any]:
        self.reply_calls.append(kwargs)
        return self.reply_pages.pop(0)

    def conversations_list(self, **kwargs: Any) -> dict[str, Any]:
        self.list_calls.append(kwargs)
        return self.list_pages.pop(0)

    def users_list(self, **kwargs: Any) -> dict[str, Any]:
        self.users_calls.append(kwargs)
        return self.users_pages.pop(0)


def _make_client() -> tuple[SlackEtlClient, _FakeWebClient]:
    client = SlackEtlClient.__new__(SlackEtlClient)
    fake_web_client = _FakeWebClient()
    client._client = fake_web_client
    client._user_cache = {}
    client._ratelimit_deadlines = {}
    return client, fake_web_client


def test_list_etl_channels_uses_workflow_user_token_client() -> None:
    client, fake_web_client = _make_client()
    fake_web_client.list_pages = [
        {
            "channels": [
                {
                    "id": "C2",
                    "name": "research",
                    "is_private": False,
                    "is_member": False,
                    "purpose": {"value": "Research"},
                    "topic": {"value": "Ideas"},
                    "num_members": 42,
                },
                {"id": "G1", "name": "private", "is_private": True},
            ],
            "response_metadata": {"next_cursor": ""},
        }
    ]

    result = client._list_etl_channels(limit=10, force_refresh=True)

    assert fake_web_client.list_calls == [
        {
            "types": "public_channel",
            "limit": 10,
            "cursor": None,
            "exclude_archived": True,
        }
    ]
    assert result == [
        {
            "id": "C2",
            "name": "research",
            "purpose": "Research",
            "topic": "Ideas",
            "member_count": 42,
            "is_archived": False,
            "is_private": False,
            "is_member": False,
        }
    ]


def test_get_etl_channel_history_page_uses_window_and_resolves_mentions() -> None:
    client, fake_web_client = _make_client()
    client._user_cache = {"U1": "alice", "U2": "bob"}
    fake_web_client.history_pages = [
        {
            "messages": [
                {"user": "U1", "text": "first <@U2>", "ts": "200.000000"},
            ],
            "response_metadata": {"next_cursor": "cursor-2"},
        }
    ]

    result = client._get_etl_channel_history_page(
        "C123",
        limit=1,
        cursor="cursor-1",
        oldest="2026-01-01",
        latest="2026-01-02",
        inclusive=True,
    )

    assert fake_web_client.history_calls == [
        {
            "channel": "C123",
            "limit": 1,
            "cursor": "cursor-1",
            "oldest": client._normalize_ts("2026-01-01"),
            "latest": client._normalize_ts("2026-01-02"),
            "inclusive": True,
        }
    ]
    assert result["has_more"] is True
    assert result["next_cursor"] == "cursor-2"
    assert result["messages"][0]["text"] == "first @bob"


def test_get_etl_thread_replies_page_reports_user_token_auth_failures() -> None:
    client, fake_web_client = _make_client()

    def fail_replies(**kwargs: Any) -> dict[str, Any]:
        raise _FakeSlackError(error="missing_scope", status_code=403)

    fake_web_client.conversations_replies = fail_replies  # type: ignore[method-assign]

    with pytest.raises(SlackEtlAuthError) as excinfo:
        client._get_etl_thread_replies_page("C123", "100.000000")

    payload = json.loads(str(excinfo.value))
    assert payload["access_path"] == "user_token"
    assert payload["slack_method"] == "conversations.replies"
    assert payload["error_code"] == "missing_scope"


def test_retry_on_ratelimit_honors_short_retry_after(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client, _ = _make_client()
    sleeps: list[float] = []
    attempts = 0
    now = 1_000.0

    def fake_sleep(seconds: float) -> None:
        nonlocal now
        sleeps.append(seconds)
        now += seconds

    def fake_time() -> float:
        return now

    def rate_limited_once() -> dict[str, Any]:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise _FakeSlackError(
                error="ratelimited",
                status_code=429,
                headers={"Retry-After": "2"},
            )
        return {"ok": True}

    monkeypatch.setattr("workflows.slack_sync_shared.time.sleep", fake_sleep)
    monkeypatch.setattr("workflows.slack_sync_shared.time.time", fake_time)

    result = client._retry_on_ratelimit(
        rate_limited_once,
        method_key="etl.conversations.history",
    )

    assert result == {"ok": True}
    assert attempts == 2
    assert sleeps == [2.25]
