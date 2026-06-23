from __future__ import annotations

import threading

import pytest

from centaur_sdk import (
    ToolContext,
    current_session_context,
    current_slack_thread,
    reset_tool_context,
    secret,
    set_tool_context,
)
from centaur_sdk.backends import registry
from centaur_sdk.backends.base import SecretBackend
from centaur_sdk.backends.env import EnvBackend
from centaur_sdk.backends.stub import StubBackend


class MappingBackend(SecretBackend):
    def __init__(self, values: dict[str, str | None]):
        self.values = values
        self.get_thread_ids: list[int] = []

    async def get(self, key: str) -> str | None:
        self.get_thread_ids.append(threading.get_ident())
        return self.values.get(key)

    async def list_keys(self) -> list[str]:
        return sorted(k for k, v in self.values.items() if v is not None)


def test_secret_prefers_tool_context_over_backend(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(registry, "_backend", MappingBackend({"TOKEN": "backend"}))
    token = set_tool_context(
        ToolContext(name="fake-tool", secrets={"TOKEN": "from-context"})
    )
    try:
        assert secret("TOKEN") == "from-context"
    finally:
        reset_tool_context(token)


def test_secret_uses_backend_when_context_is_missing(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(registry, "_backend", MappingBackend({"TOKEN": "from-backend"}))

    assert secret("TOKEN") == "from-backend"


def test_secret_uses_default_after_context_and_backend_miss(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setattr(registry, "_backend", MappingBackend({"OTHER": "value"}))
    token = set_tool_context(ToolContext(name="fake-tool", secrets={}))
    try:
        assert secret("TOKEN", default="fallback") == "fallback"
    finally:
        reset_tool_context(token)


def test_secret_raises_key_error_with_tool_name_after_all_sources_miss(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setattr(registry, "_backend", MappingBackend({}))
    token = set_tool_context(ToolContext(name="fake-tool", secrets={}))
    try:
        with pytest.raises(KeyError, match="Missing secret 'TOKEN' for tool 'fake-tool'"):
            secret("TOKEN")
    finally:
        reset_tool_context(token)


def test_current_session_context_fetches_api_context(monkeypatch: pytest.MonkeyPatch):
    requested: dict[str, str] = {}

    class FakeResponse:
        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc, tb):
            return False

        def read(self) -> bytes:
            return (
                b'{"thread_key":"slack:C123:123.456",'
                b'"slack":{"channel_id":"C123","thread_ts":"123.456"}}'
            )

    def fake_urlopen(request, timeout):
        requested["url"] = request.full_url
        requested["timeout"] = str(timeout)
        return FakeResponse()

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)
    token = set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key="slack:C123:123.456",
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )
    try:
        context = current_session_context()
        assert context["slack"]["channel_id"] == "C123"
        assert requested["url"] == "http://api:8000/api/session/slack%3AC123%3A123.456"
        assert requested["timeout"] == "30"
    finally:
        reset_tool_context(token)


def test_current_slack_thread_returns_api_slack_destination(
    monkeypatch: pytest.MonkeyPatch,
):
    class FakeResponse:
        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc, tb):
            return False

        def read(self) -> bytes:
            return (
                b'{"thread_key":"slack:C123:123.456",'
                b'"slack":{"channel_id":"C123","thread_ts":"123.456"}}'
            )

    monkeypatch.setattr("urllib.request.urlopen", lambda _request, timeout: FakeResponse())
    token = set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key="slack:C123:123.456",
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )
    try:
        assert current_slack_thread() == {"channel_id": "C123", "thread_ts": "123.456"}
    finally:
        reset_tool_context(token)


@pytest.mark.asyncio
async def test_stub_backend_returns_key_placeholders():
    backend = StubBackend()

    assert await backend.get("ALCHEMY_API_KEY") == "ALCHEMY_API_KEY"
    assert await backend.list_keys() == []


@pytest.mark.asyncio
async def test_env_backend_reads_environment(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CENTAUR_SDK_TEST_TOKEN", "from-env")
    monkeypatch.delenv("CENTAUR_SDK_MISSING_TOKEN", raising=False)
    backend = EnvBackend()

    assert await backend.get("CENTAUR_SDK_TEST_TOKEN") == "from-env"
    assert await backend.get("CENTAUR_SDK_MISSING_TOKEN") is None
    assert "CENTAUR_SDK_TEST_TOKEN" in await backend.list_keys()


def test_registry_auto_configures_stub_backend(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(registry, "_backend", None)

    backend = registry.get_backend()

    assert isinstance(backend, StubBackend)
    assert backend.get_sync("OPENAI_API_KEY") == "OPENAI_API_KEY"


def test_get_sync_runs_coroutine_on_current_thread_without_running_loop():
    backend = MappingBackend({"TOKEN": "outside-loop"})
    caller_thread_id = threading.get_ident()

    assert backend.get_sync("TOKEN") == "outside-loop"
    assert backend.get_thread_ids == [caller_thread_id]


@pytest.mark.asyncio
async def test_get_sync_uses_background_thread_inside_running_loop():
    backend = MappingBackend({"TOKEN": "inside-loop"})
    caller_thread_id = threading.get_ident()

    assert backend.get_sync("TOKEN") == "inside-loop"
    assert len(backend.get_thread_ids) == 1
    assert backend.get_thread_ids[0] != caller_thread_id
