from __future__ import annotations

import asyncio
import tomllib
import warnings
from pathlib import Path
from typing import get_args

import httpx
import pytest
import respx
from centaur_tool_websearch import _parallel, _tako
from centaur_tool_websearch import client as client_module
from centaur_tool_websearch._tako import TakoBackend
from centaur_tool_websearch.client import WebSearchClient
from centaur_tool_websearch.models import (
    DeepResearchResult,
    ResearchEffort,
    RetrievalResult,
    SearchEffort,
    SourceDocument,
)

from centaur_sdk.backends import StubBackend, configure
from centaur_sdk.backends.env import EnvBackend


@pytest.fixture(autouse=True)
def _unset_tool_secrets(monkeypatch: pytest.MonkeyPatch) -> None:
    for key in ("TAKO_API_KEY", "PARALLEL_API_KEY", "ANTHROPIC_API_KEY"):
        monkeypatch.delenv(key, raising=False)


def _doc(i: int) -> SourceDocument:
    return SourceDocument(source_id=i, title=f"t{i}", url=f"https://e.example/{i}", snippet="s")


class RecordingBackend:
    def __init__(self, result: RetrievalResult) -> None:
        self.result = result
        self.requests = []

    async def search(self, request):
        self.requests.append(request)
        return self.result

    async def deep_research(self, request, progress):
        raise AssertionError("not used")


def test_client_uses_stub_to_enable_proxy_injection() -> None:
    configure(StubBackend())

    client = WebSearchClient(backend="parallel")

    assert client._parallel_api_key == "PARALLEL_API_KEY"
    assert client._tako_api_key == "TAKO_API_KEY"


def test_an_exported_but_empty_key_still_takes_the_keyed_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    configure(EnvBackend())
    monkeypatch.setenv("TAKO_API_KEY", "")

    client = WebSearchClient(backend="tako")

    assert client._tako_api_key == "TAKO_API_KEY"
    assert client._backend.search_mode == "api"


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


@pytest.mark.parametrize(
    ("raw", "expected"),
    [("", "tako"), ("   ", "tako"), (" Tako ", "tako"), ("PARALLEL", "parallel")],
)
def test_backend_env_is_trimmed_lowercased_and_defaulted(
    monkeypatch: pytest.MonkeyPatch, raw: str, expected: str
) -> None:
    monkeypatch.setenv(client_module.WEBSEARCH_BACKEND_ENV, raw)
    configure(StubBackend())

    assert WebSearchClient().backend_name == expected


def test_synthesis_skipped_for_no_sources_is_distinguishable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    client._backend = RecordingBackend(RetrievalResult(sources=[], backend="fake"))

    result = asyncio.run(client.search("q"))

    assert result["answer_markdown"] is None
    assert any(
        "retrieval returned no sources" in f["error"] for f in result["meta"]["partial_failures"]
    )


def test_unknown_backend_name_raises_at_construction(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(client_module.WEBSEARCH_BACKEND_ENV, "exa")
    with pytest.raises(RuntimeError, match="WEBSEARCH_BACKEND='exa' is not supported"):
        WebSearchClient()


def test_deep_research_reports_missing_injected_auth(monkeypatch: pytest.MonkeyPatch) -> None:
    class FakeAuthenticationError(Exception):
        pass

    def reject_client(**_kwargs):
        raise FakeAuthenticationError

    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    monkeypatch.setattr(_parallel, "AuthenticationError", FakeAuthenticationError)
    monkeypatch.setattr(client._backend, "_sdk_client", reject_client)

    with pytest.raises(
        RuntimeError, match="deep_research requires a valid, granted PARALLEL_API_KEY"
    ):
        asyncio.run(client.deep_research("question"))


def test_search_caps_results_and_records_skipped_synthesis() -> None:
    configure(StubBackend())
    client = WebSearchClient(backend="parallel", anthropic_api_key=None)
    client._has_anthropic_key = False
    backend = RecordingBackend(
        RetrievalResult(sources=[_doc(i) for i in range(5)], backend="fake", request_ids=["r"])
    )
    client._backend = backend

    result = asyncio.run(client.search("q", num_results=2, effort="fast"))

    assert [r["source_id"] for r in result["results"]] == [0, 1]
    assert result["answer_markdown"] is None
    assert any(
        "ANTHROPIC_API_KEY is not set" in f["error"] for f in result["meta"]["partial_failures"]
    )
    assert backend.requests[0].effort == "fast"
    assert backend.requests[0].num_results == 2
    assert result["meta"]["request_ids"] == ["r"]
    assert result["meta"]["exa_request_ids"] == ["r"]


def test_search_appends_attribution_footer_to_synthesis() -> None:
    class FakePipeline:
        async def synthesize(self, **kwargs):
            assert kwargs["max_report_chars"] < 12000
            return {
                "report": "Report [0]\n\n## Sources\n[0] t0 — https://e.example/0",
                "validation_error": None,
            }

    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    client._backend = RecordingBackend(
        RetrievalResult(sources=[_doc(0)], backend="fake", attribution="Powered by X")
    )
    client._build_synthesis_pipeline = lambda: FakePipeline()

    result = asyncio.run(client.search("q"))

    assert result["answer_markdown"].endswith("\n\n---\n_Powered by X_\n")
    assert result["meta"]["attribution"] == "Powered by X"


def test_synthesis_validation_error_is_a_partial_failure_not_an_exception() -> None:
    class FakePipeline:
        async def synthesize(self, **_kwargs):
            return {"report": "draft", "validation_error": "bad ids"}

    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    client._backend = RecordingBackend(RetrievalResult(sources=[_doc(0)], backend="fake"))
    client._build_synthesis_pipeline = lambda: FakePipeline()

    result = asyncio.run(client.search("q"))

    assert result["answer_markdown"] == "draft"
    assert any(
        "synthesis failed: bad ids" in f["error"] for f in result["meta"]["partial_failures"]
    )


@pytest.mark.parametrize(("mode", "effort"), [("basic", "instant"), ("advanced", "fast")])
def test_mode_is_translated_to_effort_with_warning(mode: str, effort: str) -> None:
    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    backend = RecordingBackend(RetrievalResult(sources=[], backend="fake"))
    client._backend = backend

    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        asyncio.run(client.search("q", mode=mode, synthesize=False))

    assert backend.requests[0].effort == effort
    assert any(issubclass(w.category, DeprecationWarning) for w in caught)


def test_mode_and_effort_together_is_an_error() -> None:
    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    with pytest.raises(ValueError, match=r"either --effort or --mode"):
        asyncio.run(client.search("q", mode="basic", effort="fast"))


def test_every_effort_has_a_price_and_a_vendor_mapping() -> None:
    search_efforts = set(get_args(SearchEffort))
    research_efforts = set(get_args(ResearchEffort))

    assert set(client_module.SEARCH_EFFORTS) == search_efforts
    assert set(client_module.RESEARCH_EFFORTS) == research_efforts
    assert search_efforts <= set(_tako.SEARCH_PRICE_USD)
    assert search_efforts <= set(_parallel.EFFORT_TO_SEARCH_MODE)
    assert research_efforts <= set(_parallel.EFFORT_TO_PROCESSOR)


def test_unknown_effort_is_a_clear_error() -> None:
    configure(StubBackend())
    client = WebSearchClient(backend="parallel")
    with pytest.raises(ValueError, match="effort must be one of"):
        asyncio.run(client.search("q", effort="ultra"))


def test_default_backend_is_tako(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(client_module.WEBSEARCH_BACKEND_ENV, raising=False)
    configure(StubBackend())

    client = WebSearchClient()

    assert client.backend_name == "tako"
    assert isinstance(client._backend, TakoBackend)
    assert client._backend._api_key == "TAKO_API_KEY"


def test_tako_secret_is_injected_on_tako_com_only() -> None:
    manifest = tomllib.loads(Path(__file__).with_name("pyproject.toml").read_text())
    centaur = manifest["tool"]["centaur"]
    tako = next(
        secret for secret in centaur["optional_secrets"] if secret["name"] == "TAKO_API_KEY"
    )

    assert tako == {
        "type": "http",
        "name": "TAKO_API_KEY",
        "mode": "inject",
        "inject_header": "X-API-Key",
        "hosts": ["tako.com"],
    }
    assert centaur["hosts"] == [
        "tako.com",
        "mcp.tako.com",
        "api.parallel.ai",
        "search.parallel.ai",
        "api.anthropic.com",
    ]


@pytest.mark.parametrize(
    ("backend_name", "granted", "expected"),
    [
        ("tako", True, "tako:api"),
        ("tako", False, "tako:anonymous"),
        ("parallel", True, "parallel:api"),
        ("parallel", False, "parallel:mcp"),
    ],
)
def test_routing_matrix_never_crosses_vendors(
    monkeypatch: pytest.MonkeyPatch,
    request: pytest.FixtureRequest,
    backend_name: str,
    granted: bool,
    expected: str,
) -> None:
    monkeypatch.setenv(client_module.WEBSEARCH_BACKEND_ENV, backend_name)
    configure(StubBackend())
    client = WebSearchClient()

    if backend_name == "tako":
        router = respx.mock(assert_all_called=False)
        router.post("https://tako.com/api/v3/search").mock(
            return_value=httpx.Response(
                200 if granted else 401,
                json={"request_id": "r", "cards": [], "web_results": []},
            )
        )
        router.post("https://mcp.tako.com/mcp").mock(
            return_value=httpx.Response(
                200,
                json={
                    "jsonrpc": "2.0",
                    "id": "1",
                    "result": {"structuredContent": {"cards": [], "web_results": []}},
                },
            )
        )
        router.start()
        request.addfinalizer(router.stop)
    else:

        class FakeAuthenticationError(Exception):
            pass

        async def rest(**_kwargs):
            if not granted:
                raise FakeAuthenticationError
            return [], "r", []

        async def mcp(**_kwargs):
            if granted:
                raise AssertionError("MCP must not run when injected auth succeeds")
            return [], "m", []

        monkeypatch.setattr(_parallel, "AuthenticationError", FakeAuthenticationError)
        monkeypatch.setattr(client._backend, "_search_api", rest)
        monkeypatch.setattr(client._backend, "_search_mcp", mcp)

    result = asyncio.run(client.search("q", synthesize=False))

    assert result["meta"]["backend"] == expected
    if backend_name == "tako":
        assert {call.request.url.host for call in router.calls} <= {"tako.com", "mcp.tako.com"}


def test_deep_research_response_from_backend_result() -> None:
    class FakeBackend:
        async def search(self, request):
            raise AssertionError

        async def deep_research(self, request, progress):
            progress("working")
            return DeepResearchResult(
                sources=[_doc(1)],
                answer_markdown="report",
                backend="tako:agent",
                request_ids=["run-1"],
                usage=[{"total_cost_usd": 0.4}],
                estimated_cost_usd=0.4,
                partial_failures=[{"query": request.question, "error": "note"}],
            )

    configure(StubBackend())
    client = WebSearchClient()
    client._backend = FakeBackend()
    stages: list[str] = []
    client._set_progress_callback(stages.append)

    result = asyncio.run(client.deep_research("  Why?  ", effort="high"))

    assert result["question"] == "Why?"
    assert result["answer_markdown"] == "report"
    assert result["meta"]["backend"] == "tako:agent"
    assert result["meta"]["request_ids"] == ["run-1"]
    assert result["meta"]["estimated_cost_usd"] == 0.4
    assert result["meta"]["partial_failures"] == [{"query": "Why?", "error": "note"}]
    assert result["iterations"][0]["results_count"] == 1
    assert stages == ["working"]


def test_deep_research_rejects_unknown_effort() -> None:
    configure(StubBackend())
    client = WebSearchClient()
    with pytest.raises(ValueError, match="effort must be one of medium, high"):
        asyncio.run(client.deep_research("q", effort="ultra"))
