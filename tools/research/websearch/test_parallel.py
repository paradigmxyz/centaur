from __future__ import annotations

import asyncio

import pytest
from centaur_tool_websearch import _parallel
from centaur_tool_websearch._parallel import ParallelBackend
from centaur_tool_websearch.models import (
    DeepResearchResult,
    DeepResearchSpec,
    RetrievalResult,
    SearchRequestSpec,
    SourceDocument,
)


def _doc(i: int) -> SourceDocument:
    return SourceDocument(source_id=i, title=f"t{i}", url=f"https://e.example/{i}")


def test_search_rest_returns_retrieval_result(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict = {}

    async def search_rest(**kwargs):
        captured.update(kwargs)
        return [_doc(0)], "req-1", [{"cost": 1}]

    backend = ParallelBackend(api_key="PARALLEL_API_KEY")
    monkeypatch.setattr(backend, "_search_api", search_rest)

    result = asyncio.run(
        backend.search(SearchRequestSpec(query="q", effort="instant", num_results=3))
    )

    assert isinstance(result, RetrievalResult)
    assert result.backend == "parallel:api"
    assert result.request_ids == ["req-1"]
    assert result.usage == [{"cost": 1}]
    assert result.attribution is None
    assert result.estimated_cost_usd == _parallel._estimate_search_cost_usd(3)
    assert captured["mode"] == "basic"
    assert captured["search_queries"] == ["q"]


@pytest.mark.parametrize(
    ("effort", "mode", "noted"),
    [
        (None, None, False),
        ("instant", "basic", False),
        ("fast", "advanced", False),
        ("deep", "advanced", True),
    ],
)
def test_search_effort_maps_to_parallel_mode(
    monkeypatch: pytest.MonkeyPatch, effort: str | None, mode: str | None, noted: bool
) -> None:
    captured: dict = {}

    async def search_rest(**kwargs):
        captured.update(kwargs)
        return [], "", []

    backend = ParallelBackend(api_key="PARALLEL_API_KEY")
    monkeypatch.setattr(backend, "_search_api", search_rest)

    result = asyncio.run(backend.search(SearchRequestSpec(query="q", effort=effort)))

    assert captured["mode"] == mode
    assert any("deep" in f["error"] for f in result.partial_failures) is noted


def test_search_falls_back_to_mcp_with_attribution(monkeypatch: pytest.MonkeyPatch) -> None:
    class FakeAuthenticationError(Exception):
        pass

    async def reject_rest(**_kwargs):
        raise FakeAuthenticationError

    async def search_mcp(**_kwargs):
        return [_doc(0)], "mcp-1", []

    backend = ParallelBackend(api_key="PARALLEL_API_KEY")
    monkeypatch.setattr(_parallel, "AuthenticationError", FakeAuthenticationError)
    monkeypatch.setattr(backend, "_search_api", reject_rest)
    monkeypatch.setattr(backend, "_search_mcp", search_mcp)

    result = asyncio.run(backend.search(SearchRequestSpec(query="q", include_domains=["a.com"])))

    assert result.backend == "parallel:mcp"
    assert result.attribution == _parallel._FREE_MCP_ATTRIBUTION
    assert result.estimated_cost_usd == 0.0
    assert backend.search_mode == "mcp"
    assert backend._mcp_headers(None).get("Authorization") is None
    errors = " ".join(f["error"] for f in result.partial_failures)
    assert "did not authenticate" in errors
    assert "include_domains" in errors


@pytest.mark.parametrize(
    ("effort", "processor", "expected"),
    [
        (None, None, "ultra-fast"),
        ("medium", None, "ultra-fast"),
        ("high", None, "ultra"),
        ("high", "pro-fast", "pro-fast"),
    ],
)
def test_deep_research_effort_and_processor_mapping(
    monkeypatch: pytest.MonkeyPatch, effort: str | None, processor: str | None, expected: str
) -> None:
    seen: dict = {}

    async def fake_run(self, *, question, processor, timeout_seconds, progress, max_report_chars):
        seen["processor"] = processor
        seen["timeout"] = timeout_seconds
        return "run-1", ([_doc(1)], "report\n\n## Sources\n[1] t1 — https://e.example/1")

    backend = ParallelBackend(api_key="PARALLEL_API_KEY")
    monkeypatch.setattr(ParallelBackend, "_run_task", fake_run)

    result = asyncio.run(
        backend.deep_research(
            DeepResearchSpec(question="why", effort=effort, processor=processor), lambda _s: None
        )
    )

    assert isinstance(result, DeepResearchResult)
    assert seen["processor"] == expected
    assert seen["timeout"] == _parallel.PROCESSOR_TIMEOUT_SECONDS[expected]
    assert result.backend == f"parallel:task:{expected}"
    assert result.request_ids == ["run-1"]
    assert result.estimated_cost_usd == _parallel._estimate_task_cost_usd(expected)


def test_deep_research_rejects_unknown_processor() -> None:
    backend = ParallelBackend(api_key="PARALLEL_API_KEY")
    with pytest.raises(RuntimeError, match="pro/ultra processor"):
        asyncio.run(
            backend.deep_research(
                DeepResearchSpec(question="why", processor="lite"), lambda _s: None
            )
        )


def test_deep_research_requires_key() -> None:
    backend = ParallelBackend(api_key=None)
    with pytest.raises(RuntimeError, match="requires PARALLEL_API_KEY"):
        asyncio.run(backend.deep_research(DeepResearchSpec(question="why"), lambda _s: None))
