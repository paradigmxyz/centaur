from __future__ import annotations

import asyncio
import tomllib
from pathlib import Path

import pytest
from centaur_tool_websearch import _parallel
from centaur_tool_websearch.client import WebSearchClient

from centaur_sdk.backends import StubBackend, configure


def test_client_uses_stub_to_enable_proxy_injection() -> None:
    configure(StubBackend())

    client = WebSearchClient()

    assert client._parallel_api_key == "PARALLEL_API_KEY"
    assert client.has_api_key is True


def test_parallel_secret_is_injected_into_sdk_header() -> None:
    manifest = tomllib.loads(Path(__file__).with_name("pyproject.toml").read_text())
    secrets = manifest["tool"]["centaur"]["optional_secrets"]
    parallel = next(secret for secret in secrets if secret["name"] == "PARALLEL_API_KEY")

    assert parallel == {
        "type": "http",
        "name": "PARALLEL_API_KEY",
        "mode": "inject",
        "inject_header": "x-api-key",
        "hosts": ["api.parallel.ai"],
    }


def test_search_attempts_rest_with_inject_stub(monkeypatch: pytest.MonkeyPatch) -> None:
    async def search_rest(**_kwargs):
        return [], "api-request", []

    async def reject_mcp(**_kwargs):
        raise AssertionError("MCP should not be used when injected auth succeeds")

    configure(StubBackend())
    client = WebSearchClient()
    monkeypatch.setattr(client._backend, "_search_api", search_rest)
    monkeypatch.setattr(client._backend, "_search_mcp", reject_mcp)

    result = asyncio.run(client.search("injected", synthesize=False))

    assert result["meta"]["backend"] == "parallel:api"


def test_search_falls_back_to_anonymous_mcp_when_injected_auth_is_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakeAuthenticationError(Exception):
        pass

    async def reject_rest(**_kwargs):
        raise FakeAuthenticationError

    async def search_mcp(**_kwargs):
        return [], "mcp-request", []

    configure(StubBackend())
    client = WebSearchClient()
    monkeypatch.setattr(_parallel, "AuthenticationError", FakeAuthenticationError)
    monkeypatch.setattr(client._backend, "_search_api", reject_rest)
    monkeypatch.setattr(client._backend, "_search_mcp", search_mcp)

    result = asyncio.run(client.search("fallback", synthesize=False))

    assert result["meta"]["backend"] == "parallel:mcp"
    assert client.search_mode == "mcp"
    assert client._backend._mcp_headers(None).get("Authorization") is None


def test_deep_research_reports_missing_injected_auth(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakeAuthenticationError(Exception):
        pass

    def reject_client(**_kwargs):
        raise FakeAuthenticationError

    configure(StubBackend())
    client = WebSearchClient()
    monkeypatch.setattr(_parallel, "AuthenticationError", FakeAuthenticationError)
    monkeypatch.setattr(client._backend, "_sdk_client", reject_client)

    with pytest.raises(
        RuntimeError,
        match="deep_research requires a valid, granted PARALLEL_API_KEY",
    ):
        asyncio.run(client.deep_research("question"))
