"""Tests for slackbot_client transient-error retry behavior."""

from __future__ import annotations

import json
from typing import Any
from unittest.mock import patch

import httpx
import pytest


@pytest.fixture(autouse=True)
def _slackbot_env(monkeypatch):
    monkeypatch.setenv("SLACKBOT_URL", "http://slackbot.test")
    monkeypatch.setenv("SLACKBOT_API_KEY", "test-key")


@pytest.fixture(autouse=True)
def _no_sleep(monkeypatch):
    async def _instant(_s: float) -> None:
        return None

    monkeypatch.setattr("api.slackbot_client.asyncio.sleep", _instant)


def _response(status: int, body: dict[str, Any] | None = None) -> httpx.Response:
    text = json.dumps(body) if body is not None else ""
    return httpx.Response(status_code=status, text=text)


class _FakeClient:
    def __init__(self, responses: list[httpx.Response]) -> None:
        self._responses = list(responses)
        self.calls: list[dict[str, Any]] = []

    async def __aenter__(self) -> "_FakeClient":
        return self

    async def __aexit__(self, *_: Any) -> None:
        return None

    async def post(self, url: str, **kwargs: Any) -> httpx.Response:
        self.calls.append({"url": url, **kwargs})
        if not self._responses:
            raise RuntimeError("no response programmed")
        return self._responses.pop(0)


@pytest.mark.asyncio
async def test_post_retries_on_5xx_then_returns_payload():
    fake = _FakeClient([_response(502), _response(503), _response(200, {"ok": True})])
    with patch("api.slackbot_client.httpx.AsyncClient", return_value=fake):
        from api import slackbot_client

        result = await slackbot_client.post(
            "/api/slack/agent-sessions/sess/text", {"markdown": "x"}
        )

    assert result == {"ok": True}
    assert len(fake.calls) == 3


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [408, 429])
async def test_post_retries_on_retryable_4xx(status: int):
    fake = _FakeClient([_response(status), _response(200, {"ok": True})])
    with patch("api.slackbot_client.httpx.AsyncClient", return_value=fake):
        from api import slackbot_client

        result = await slackbot_client.post("/api/slack/agent-sessions/sess/done", {})

    assert result == {"ok": True}
    assert len(fake.calls) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [400, 403, 404])
async def test_post_does_not_retry_on_permanent_4xx(status: int):
    fake = _FakeClient([_response(status, {"error": "bad"})])
    with patch("api.slackbot_client.httpx.AsyncClient", return_value=fake):
        from api import slackbot_client

        result = await slackbot_client.post("/api/slack/agent-sessions/sess/done", {})

    assert result is None
    assert len(fake.calls) == 1


@pytest.mark.asyncio
async def test_post_returns_none_after_exhausting_retries():
    fake = _FakeClient([_response(502), _response(502), _response(502)])
    with patch("api.slackbot_client.httpx.AsyncClient", return_value=fake):
        from api import slackbot_client

        result = await slackbot_client.post(
            "/api/slack/agent-sessions/sess/text", {"markdown": "x"}
        )

    assert result is None
    assert len(fake.calls) == 3
