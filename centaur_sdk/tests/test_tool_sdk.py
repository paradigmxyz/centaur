from __future__ import annotations

import threading
from pathlib import Path

import pytest

from centaur_sdk import (
    ToolContext,
    current_chat_destination,
    current_discord_thread,
    current_github_thread,
    current_linear_thread,
    current_session_context,
    current_slack_thread,
    reset_tool_context,
    save_attachment,
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


def test_current_session_context_requires_api_server_capability(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setattr(
        registry,
        "_backend",
        MappingBackend({"CENTAUR_SANDBOX_API_SERVER_ENABLED": "false"}),
    )
    token = set_tool_context(ToolContext(name="fake-tool", thread_key="slack:C123:123.456"))
    try:
        with pytest.raises(RuntimeError, match="API server sandbox capability"):
            current_session_context()
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


def _fake_context_response(payload: bytes):
    class FakeResponse:
        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc, tb):
            return False

        def read(self) -> bytes:
            return payload

    return FakeResponse


def _discord_context(thread_key: str, monkeypatch: pytest.MonkeyPatch):
    payload = (
        b'{"thread_key":"' + thread_key.encode() + b'","platform":"discord",'
        b'"discord":{"guild_id":"111","channel_id":"222","thread_id":"333"}}'
    )
    monkeypatch.setattr(
        "urllib.request.urlopen", lambda _request, timeout: _fake_context_response(payload)()
    )
    return set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key=thread_key,
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )


def test_current_discord_thread_returns_api_discord_destination(
    monkeypatch: pytest.MonkeyPatch,
):
    token = _discord_context("discord:111:222:333", monkeypatch)
    try:
        assert current_discord_thread() == {
            "guild_id": "111",
            "channel_id": "222",
            "thread_id": "333",
        }
    finally:
        reset_tool_context(token)


def test_current_chat_destination_tags_platform(monkeypatch: pytest.MonkeyPatch):
    token = _discord_context("discord:111:222:333", monkeypatch)
    try:
        assert current_chat_destination() == {
            "platform": "discord",
            "guild_id": "111",
            "channel_id": "222",
            "thread_id": "333",
        }
    finally:
        reset_tool_context(token)


def _linear_context(thread_key: str, monkeypatch: pytest.MonkeyPatch):
    payload = (
        b'{"thread_key":"' + thread_key.encode() + b'","platform":"linear",'
        b'"linear":{"issue_id":"ISSUE","comment_id":"CMT","agent_session_id":"SESS"}}'
    )
    monkeypatch.setattr(
        "urllib.request.urlopen", lambda _request, timeout: _fake_context_response(payload)()
    )
    return set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key=thread_key,
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )


def test_current_linear_thread_returns_api_linear_destination(
    monkeypatch: pytest.MonkeyPatch,
):
    token = _linear_context("linear:ISSUE:c:CMT:s:SESS", monkeypatch)
    try:
        assert current_linear_thread() == {
            "issue_id": "ISSUE",
            "comment_id": "CMT",
            "agent_session_id": "SESS",
        }
    finally:
        reset_tool_context(token)


def test_current_chat_destination_tags_linear_platform(monkeypatch: pytest.MonkeyPatch):
    token = _linear_context("linear:ISSUE:c:CMT:s:SESS", monkeypatch)
    try:
        assert current_chat_destination() == {
            "platform": "linear",
            "issue_id": "ISSUE",
            "comment_id": "CMT",
            "agent_session_id": "SESS",
        }
    finally:
        reset_tool_context(token)


def _github_context(thread_key: str, monkeypatch: pytest.MonkeyPatch):
    payload = (
        b'{"thread_key":"' + thread_key.encode() + b'","platform":"github",'
        b'"github":{"owner":"0xSplits","repo":"centaur","number":704,'
        b'"kind":"pr","review_comment_id":99}}'
    )
    monkeypatch.setattr(
        "urllib.request.urlopen", lambda _request, timeout: _fake_context_response(payload)()
    )
    return set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key=thread_key,
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )


def test_current_github_thread_returns_api_github_destination(
    monkeypatch: pytest.MonkeyPatch,
):
    token = _github_context("github:0xSplits/centaur:704:rc:99", monkeypatch)
    try:
        assert current_github_thread() == {
            "owner": "0xSplits",
            "repo": "centaur",
            "number": 704,
            "kind": "pr",
            "review_comment_id": 99,
        }
    finally:
        reset_tool_context(token)


def test_current_chat_destination_tags_github_platform(monkeypatch: pytest.MonkeyPatch):
    token = _github_context("github:0xSplits/centaur:704:rc:99", monkeypatch)
    try:
        assert current_chat_destination() == {
            "platform": "github",
            "owner": "0xSplits",
            "repo": "centaur",
            "number": 704,
            "kind": "pr",
            "review_comment_id": 99,
        }
    finally:
        reset_tool_context(token)


def test_current_github_thread_rejects_slack_thread(monkeypatch: pytest.MonkeyPatch):
    payload = (
        b'{"thread_key":"slack:C123:123.456","platform":"slack",'
        b'"slack":{"channel_id":"C123","thread_ts":"123.456"}}'
    )
    monkeypatch.setattr(
        "urllib.request.urlopen", lambda _request, timeout: _fake_context_response(payload)()
    )
    token = set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key="slack:C123:123.456",
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )
    try:
        with pytest.raises(RuntimeError, match="not a GitHub thread"):
            current_github_thread()
    finally:
        reset_tool_context(token)


def test_current_linear_thread_rejects_slack_thread(monkeypatch: pytest.MonkeyPatch):
    payload = (
        b'{"thread_key":"slack:C123:123.456","platform":"slack",'
        b'"slack":{"channel_id":"C123","thread_ts":"123.456"}}'
    )
    monkeypatch.setattr(
        "urllib.request.urlopen", lambda _request, timeout: _fake_context_response(payload)()
    )
    token = set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key="slack:C123:123.456",
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )
    try:
        with pytest.raises(RuntimeError, match="not a Linear thread"):
            current_linear_thread()
    finally:
        reset_tool_context(token)


def test_current_discord_thread_rejects_slack_thread(monkeypatch: pytest.MonkeyPatch):
    payload = (
        b'{"thread_key":"slack:C123:123.456","platform":"slack",'
        b'"slack":{"channel_id":"C123","thread_ts":"123.456"}}'
    )
    monkeypatch.setattr(
        "urllib.request.urlopen", lambda _request, timeout: _fake_context_response(payload)()
    )
    token = set_tool_context(
        ToolContext(
            name="fake-tool",
            thread_key="slack:C123:123.456",
            secrets={"CENTAUR_API_URL": "http://api:8000", "CENTAUR_API_KEY": ""},
        )
    )
    try:
        with pytest.raises(RuntimeError, match="not a Discord thread"):
            current_discord_thread()
    finally:
        reset_tool_context(token)


def test_save_attachment_writes_to_sandbox_uploads_dir(
    monkeypatch: pytest.MonkeyPatch, tmp_path
):
    def fail_urlopen(*_args, **_kwargs):
        raise AssertionError("save_attachment should not call the API in sandbox mode")

    monkeypatch.setenv("CENTAUR_UPLOADS_DIR", str(tmp_path))
    monkeypatch.setattr("urllib.request.urlopen", fail_urlopen)

    result = save_attachment(
        name="../report.txt",
        data=b"hello",
        mime_type="text/plain",
        source_url="https://example.test/report",
    )

    saved_path = tmp_path / "report.txt"
    assert saved_path.read_bytes() == b"hello"
    assert result == {
        "attachment_id": None,
        "filename": "report.txt",
        "mime_type": "text/plain",
        "download_url": None,
        "path": str(saved_path),
        "local_path": str(saved_path),
        "source_url": "https://example.test/report",
        "size_bytes": 5,
    }


def test_save_attachment_requires_api_server_capability_without_uploads_dir(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.delenv("CENTAUR_UPLOADS_DIR", raising=False)
    monkeypatch.setattr(
        registry,
        "_backend",
        MappingBackend({"CENTAUR_SANDBOX_API_SERVER_ENABLED": "false"}),
    )
    token = set_tool_context(ToolContext(name="fake-tool", thread_key="slack:C123:123.456"))
    try:
        with pytest.raises(RuntimeError, match="API server sandbox capability"):
            save_attachment(name="report.txt", data=b"hello")
    finally:
        reset_tool_context(token)


def test_save_attachment_uses_unique_local_name_on_collision(
    monkeypatch: pytest.MonkeyPatch, tmp_path
):
    monkeypatch.setenv("CENTAUR_UPLOADS_DIR", str(tmp_path))

    first = save_attachment(name="same.txt", data=b"first")
    second = save_attachment(name="same.txt", data=b"second")

    assert first["path"] != second["path"]
    assert (tmp_path / "same.txt").read_bytes() == b"first"
    second_path = Path(str(second["path"]))
    assert second_path.exists()
    assert second_path.read_bytes() == b"second"
    assert second_path.name.startswith("same-")
    assert second_path.suffix == ".txt"


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
