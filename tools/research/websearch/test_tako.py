from __future__ import annotations

import asyncio
import json
from collections.abc import Iterator

import httpx
import pytest
import respx
from centaur_tool_websearch import _tako
from centaur_tool_websearch._tako import (
    AnonymousSearchOutput,
    TakoBackend,
    normalize_anonymous_output,
    normalize_answer_result,
    normalize_search_response,
)
from centaur_tool_websearch.models import DeepResearchSpec, SearchRequestSpec
from tako.aio.models import AnswerAgentResult, SearchResponse
from tako.lib import streaming
from tako.lib.caller import CALLER_HEADER, CALLER_VALUE

SEARCH_URL = "https://tako.com/api/v3/search"
MCP_URL = "https://mcp.tako.com/mcp"
RUNS_URL = "https://tako.com/api/v1/agent/answer/runs"
RUN_PATH = "/api/v1/agent/answer/runs/run-1"

V3_RESPONSE = {
    "request_id": "req-v3-1",
    "usage": {"total_cost_usd": 0.007, "compute": {"cost_usd": 0.007}},
    "cards": [
        {
            "card_id": "abc",
            "title": "United States GDP Growth Rate",
            "description": "The real GDP growth rate of United States is 2.1% as of 2025.",
            "semantic_description": "Real GDP growth for the United States over time.",
            "webpage_url": "https://tako.com/card/abc/",
            "image_url": "https://tako.com/api/v1/image/abc/",
            "embed_url": "https://tako.com/embed/abc/",
            "sources": [
                {
                    "source_name": "International Monetary Fund",
                    "source_index": "data",
                    "url": "https://imf.org",
                },
                {"source_name": "World Bank", "source_index": "data"},
            ],
            "methodologies": [
                {
                    "methodology_name": "Where the Data Comes From - IMF",
                    "methodology_description": "World Economic Outlook estimates.",
                }
            ],
            "metric_definitions": [
                {
                    "name": "Real GDP Growth Rate",
                    "definition": "Percentage change in inflation-adjusted GDP.",
                }
            ],
            "data_freshness": {"coverage_end": "2025", "last_updated": "2026-09-03"},
            "exportable": True,
            "nodes": [{"id": "ent::us::1", "type": "entity", "name": "United States"}],
        },
        {"card_id": "nourl", "title": "No page", "description": "x", "webpage_url": None},
    ],
    "web_results": [
        {
            "title": "GDP (Second Estimate) | BEA",
            "url": "https://www.bea.gov/news/2026/gdp-q2",
            "snippet": "Real GDP increased at an annual rate of 1.5 percent … in the second quarter.",
            "source_name": "U.S. Bureau of Economic Analysis",
            "publish_date": "2026-08-28",
        },
        {"title": "No publisher", "url": "https://example.org/a", "snippet": None},
    ],
}

ANONYMOUS_OUTPUT = {
    "cards": [
        {
            "exportable": True,
            "title": "United States GDP Growth Rate",
            "description": "The real gdp growth rate of United States is 2.1% as of 2025.",
            "url": "https://tako.com/card/0qsNya-V-bgimmD4QFQB/",
            "source": "International Monetary Fund",
            "coverage_end": "2025-01-01",
            "last_updated": "2026-09-03",
            "relevance": "High",
        }
    ],
    "web_results": [
        {
            "url": "https://www.bea.gov/news/2026/gdp-q2",
            "title": "GDP (Second Estimate) | BEA",
            "snippet": "Real gross domestic product (GDP) increased at an annual rate of 1.5 percent",
            "source": "U.S. Bureau of Economic Analysis",
        },
        {"url": "https://example.org/b", "title": "B", "snippet": None, "published": "2026-01-02"},
    ],
    "usage": {"total_cost_usd": 0.007, "compute": {"cost_usd": 0.007}},
    "metric_definitions": {
        "Real GDP Growth Rate": "The percentage change in real GDP over a period."
    },
    "source_notes": {
        "International Monetary Fund": "An international organization that provides economic data."
    },
}

ANSWER_RESULT = {
    "answer": "GDP grew 2.1% in 2025 [1]. Q2 2026 came in at 1.5% [2].",
    "cards": [
        {
            "title": "United States GDP Growth Rate",
            "description": "The real GDP growth rate of United States is 2.1% as of 2025.",
            "webpage_url": "https://tako.com/card/abc/",
            "sources": [{"source_name": "International Monetary Fund", "source_index": "data"}],
            "metric_definitions": [{"name": "Real GDP Growth Rate", "definition": "Pct change."}],
            "data_freshness": {"last_updated": "2026-09-03"},
        }
    ],
    "citations": [
        {
            "index": 1,
            "title": "United States GDP Growth Rate",
            "url": None,
            "source_name": "International Monetary Fund",
        },
        {
            "index": 2,
            "title": "GDP (Second Estimate) | BEA",
            "url": "https://www.bea.gov/news/2026/gdp-q2",
            "source_name": "U.S. Bureau of Economic Analysis",
            "excerpt": "1.5 percent",
            "publish_date": "2026-08-28",
        },
        {"index": 3, "title": "Unlinked data source", "url": None},
    ],
    "metadata": {
        "definitions": [
            {"term": "Real GDP", "definition": "Inflation-adjusted output.", "source_ref": 1}
        ],
        "assumptions": [
            {"title": "Calendar years", "description": "Annual figures use calendar years."}
        ],
        "methodology": [
            {"title": "Growth rate", "description": "Year-over-year percentage change."}
        ],
    },
    "refusal_code": None,
    "request_id": "rq-1",
}


@pytest.fixture
def mock() -> Iterator[respx.MockRouter]:
    with respx.mock(assert_all_called=False) as router:
        yield router


@pytest.fixture
def no_backoff(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(streaming, "_backoff_seconds", lambda _attempt: 0.0)
    monkeypatch.setattr(_tako, "POLL_INTERVAL_SECONDS", 0.0)


def _backend(api_key: str | None = "TAKO_API_KEY") -> TakoBackend:
    return TakoBackend(api_key=api_key)


def _search(backend: TakoBackend, **spec):
    return asyncio.run(backend.search(SearchRequestSpec(**spec)))


def _research(backend: TakoBackend, progress=lambda _stage: None, **spec):
    return asyncio.run(backend.deep_research(DeepResearchSpec(**spec), progress))


def _requests(mock: respx.MockRouter) -> list[httpx.Request]:
    return [call.request for call in mock.calls]


def test_v3_cards_come_first_and_carry_definitions_and_methodology() -> None:
    sources = normalize_search_response(SearchResponse.from_dict(V3_RESPONSE))

    assert [d.source_id for d in sources] == [0, 1, 2]
    card = sources[0]
    assert card.url == "https://tako.com/card/abc/"
    assert card.title == "United States GDP Growth Rate"
    assert card.domain == "International Monetary Fund, World Bank"
    assert card.published_date == "2026-09-03"
    assert "2.1% as of 2025" in card.snippet
    assert "Real GDP Growth Rate: Percentage change" in card.snippet
    assert "Where the Data Comes From - IMF: World Economic Outlook" in card.snippet


def test_v3_card_falls_back_to_semantic_description() -> None:
    payload = SearchResponse.from_dict(
        {
            **V3_RESPONSE,
            "cards": [{**V3_RESPONSE["cards"][0], "description": None}],
            "web_results": [],
        }
    )
    assert normalize_search_response(payload)[0].snippet.startswith("Real GDP growth for")


def test_v3_web_results_map_publisher_and_date() -> None:
    sources = normalize_search_response(SearchResponse.from_dict(V3_RESPONSE))

    web = sources[1]
    assert web.domain == "U.S. Bureau of Economic Analysis"
    assert web.published_date == "2026-08-28"
    assert web.snippet.startswith("Real GDP increased")
    assert sources[2].domain == "example.org"
    assert sources[2].snippet == ""


def test_anonymous_cards_use_projected_fields_and_merged_definitions() -> None:
    sources = normalize_anonymous_output(AnonymousSearchOutput.model_validate(ANONYMOUS_OUTPUT))

    assert [d.source_id for d in sources] == [0, 1, 2]
    card = sources[0]
    assert card.domain == "International Monetary Fund"
    assert card.published_date == "2026-09-03"
    assert "Real GDP Growth Rate: The percentage change" in card.snippet
    assert "International Monetary Fund: An international organization" in card.snippet
    assert sources[2].published_date == "2026-01-02"
    assert sources[2].domain == "example.org"


def test_answer_result_sources_keep_citation_indexes_and_fill_missing_urls() -> None:
    sources, _ = normalize_answer_result(
        AnswerAgentResult.from_dict(ANSWER_RESULT), max_report_chars=50_000
    )

    assert [d.source_id for d in sources] == [1, 2, 3]
    assert sources[0].url == "https://tako.com/card/abc/"
    assert sources[0].domain == "International Monetary Fund"
    assert sources[1].snippet == "1.5 percent"
    assert sources[2].url == _tako.FALLBACK_CITATION_URL


def test_answer_result_report_sections_in_order() -> None:
    _, report = normalize_answer_result(
        AnswerAgentResult.from_dict(ANSWER_RESULT), max_report_chars=50_000
    )

    order = [
        report.index(h)
        for h in ("## Charts", "## Definitions", "## Assumptions", "## Methodology", "## Sources")
    ]
    assert order == sorted(order)
    assert "- **Real GDP**: Inflation-adjusted output. [1]" in report
    assert "[2] GDP (Second Estimate) | BEA — https://www.bea.gov/news/2026/gdp-q2" in report


def test_answer_result_omits_empty_sections_and_protects_sources() -> None:
    payload = AnswerAgentResult.from_dict(
        {**ANSWER_RESULT, "cards": [], "metadata": None, "answer": "x" * 500}
    )
    sources, report = normalize_answer_result(payload, max_report_chars=200)

    assert "## Charts" not in report
    assert "## Definitions" not in report
    assert report.endswith("[3] Unlinked data source — https://tako.com"), report[-80:]
    assert [d.source_id for d in sources] == [1, 2, 3]


def test_duplicate_citation_indexes_collapse_to_one_source() -> None:
    payload = AnswerAgentResult.from_dict(
        {
            "answer": "a [1]",
            "cards": [],
            "citations": [
                {"index": 1, "title": "First", "url": "https://a.example"},
                {"index": 1, "title": "Second", "url": "https://b.example"},
            ],
        }
    )
    sources, _ = normalize_answer_result(payload, max_report_chars=5000)

    assert [d.source_id for d in sources] == [1]
    assert sources[0].title == "First"


def test_sparse_citation_indexes_survive_and_uncited_card_follows_the_highest() -> None:
    payload = AnswerAgentResult.from_dict(
        {
            **ANSWER_RESULT,
            "citations": [
                {"index": 1, "title": "St. Louis Fed", "url": "https://www.stlouisfed.org/"},
                {
                    "index": 15,
                    "title": "FRED series",
                    "url": "https://fred.stlouisfed.org/series/A191RL1A225NBEA",
                },
                {
                    "index": 22,
                    "title": "Eurostat",
                    "url": "https://ec.europa.eu/eurostat/web/products-euro-indicators",
                },
            ],
            "cards": [
                {
                    "title": "Real GDP Growth Comparison",
                    "webpage_url": "https://tako.com/card/DjDkgFurektE8dRo4JiI/",
                    "data_freshness": {"last_updated": "2026-09-03"},
                }
            ],
        }
    )

    docs, report = normalize_answer_result(payload, max_report_chars=50000)

    assert [d.source_id for d in docs] == [1, 15, 22, 23]
    assert docs[0].domain == "www.stlouisfed.org"
    assert docs[3].url == "https://tako.com/card/DjDkgFurektE8dRo4JiI/"
    assert "[15] FRED series — https://fred.stlouisfed.org/series/A191RL1A225NBEA" in report


def _rpc_ok(structured: dict) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": "1",
        "result": {
            "content": [{"type": "text", "text": "ok"}],
            "structuredContent": structured,
            "isError": False,
        },
    }


def _rpc_rate_limited() -> dict:
    return {
        "jsonrpc": "2.0",
        "id": "1",
        "result": {
            "content": [
                {"type": "text", "text": "Anonymous search is limited to 10 calls per minute."}
            ],
            "_meta": {"tako/error": {"kind": "rate_limited"}},
            "isError": True,
        },
    }


def _route_search(mock: respx.MockRouter, status: int = 200, payload: dict = V3_RESPONSE):
    body = payload if status == 200 else {"error_message": "Invalid API key"}
    return mock.post(SEARCH_URL).mock(return_value=httpx.Response(status, json=body))


def _route_mcp(mock: respx.MockRouter, response: httpx.Response | None = None):
    return mock.post(MCP_URL).mock(
        return_value=response or httpx.Response(200, json=_rpc_ok(ANONYMOUS_OUTPUT))
    )


def test_keyed_search_hits_v3_with_the_placeholder_and_the_sdk_attribution(
    mock: respx.MockRouter,
) -> None:
    _route_search(mock)

    result = _search(
        _backend(),
        query="US GDP",
        num_results=7,
        effort="deep",
        include_domains=["bea.gov"],
        max_age_hours=48,
    )

    (request,) = _requests(mock)
    assert request.url == SEARCH_URL
    assert request.headers["x-api-key"] == "TAKO_API_KEY"
    assert request.headers[CALLER_HEADER] == CALLER_VALUE
    body = json.loads(request.content)
    assert body["query"] == "US GDP"
    assert body["effort"] == "deep"
    assert body["sources"]["data"]["count"] == _tako.DATA_CARD_COUNT
    assert body["sources"]["web"]["count"] == 7
    assert body["sources"]["web"]["highlights"] is True
    assert body["sources"]["web"]["include_domains"] == ["bea.gov"]
    assert len(body["sources"]["web"]["published_after"]) == 10
    assert result.backend == "tako:api"
    assert result.request_ids == ["req-v3-1"]
    assert result.estimated_cost_usd == 0.007
    assert result.usage[0]["total_cost_usd"] == 0.007
    assert [d.source_id for d in result.sources] == [0, 1, 2]


def test_keyed_search_caps_counts_at_twenty_and_prices_deep_without_usage(
    mock: respx.MockRouter,
) -> None:
    _route_search(mock, payload={**V3_RESPONSE, "usage": None})

    result = _search(_backend(), query="q", num_results=40, effort="deep")

    body = json.loads(_requests(mock)[0].content)
    assert body["sources"]["data"]["count"] == _tako.DATA_CARD_COUNT
    assert body["sources"]["web"]["count"] == 20
    assert result.usage == []
    assert result.estimated_cost_usd == 0.012


def test_web_results_survive_the_client_cap_at_the_default_count(mock: respx.MockRouter) -> None:
    def respond(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "cards": [
                    {"title": f"c{i}", "webpage_url": f"https://tako.com/{i}", "description": "d"}
                    for i in range(body["sources"]["data"]["count"])
                ],
                "web_results": [
                    {"title": f"w{i}", "url": f"https://w.example/{i}", "snippet": "s"}
                    for i in range(body["sources"]["web"]["count"])
                ],
                "request_id": "r1",
            },
        )

    mock.post(SEARCH_URL).mock(side_effect=respond)

    result = _search(_backend(), query="q", num_results=10)

    hosts = [httpx.URL(d.url).host for d in result.sources[:10]]
    assert hosts.count("tako.com") == _tako.DATA_CARD_COUNT
    assert hosts.count("w.example") == 10 - _tako.DATA_CARD_COUNT


def test_keyed_search_notes_the_knobs_tako_ignores(mock: respx.MockRouter) -> None:
    _route_search(mock)

    result = _search(
        _backend(), query="q", max_chars_total=5000, client_model="claude-opus-4-7", session_id="s"
    )

    notes = " ".join(f["error"] for f in result.partial_failures)
    assert "max_chars_total" in notes
    assert "client_model" in notes
    assert "session_id" in notes


@pytest.mark.parametrize("status", [401, 403])
def test_a_not_granted_key_falls_back_to_anonymous_search(
    mock: respx.MockRouter, status: int
) -> None:
    _route_search(mock, status=status)
    _route_mcp(mock)

    result = _search(_backend(), query="q")

    rest, anonymous = _requests(mock)
    assert rest.url == SEARCH_URL
    assert anonymous.url == MCP_URL
    assert "x-api-key" not in anonymous.headers
    assert result.backend == "tako:anonymous"
    assert result.estimated_cost_usd == 0.0
    assert [d.source_id for d in result.sources] == [0, 1, 2]
    assert any("did not authenticate" in f["error"] for f in result.partial_failures)


def test_later_searches_skip_rest_and_keep_the_auth_note(mock: respx.MockRouter) -> None:
    _route_search(mock, status=401)
    _route_mcp(mock)
    backend = _backend()

    _search(backend, query="first")
    second = _search(backend, query="second")

    assert [str(r.url) for r in _requests(mock)] == [SEARCH_URL, MCP_URL, MCP_URL]
    assert any("did not authenticate" in f["error"] for f in second.partial_failures)


def test_non_auth_rest_error_raises_instead_of_falling_back(mock: respx.MockRouter) -> None:
    _route_search(mock, status=500)
    _route_mcp(mock)

    with pytest.raises(Exception, match="500"):
        _search(_backend(), query="a")

    assert [str(r.url) for r in _requests(mock)] == [SEARCH_URL]


def test_anonymous_search_without_a_key_notes_the_ignored_filters(mock: respx.MockRouter) -> None:
    _route_mcp(mock)

    result = _search(_backend(api_key=None), query="q", include_domains=["bea.gov"], effort="deep")

    (request,) = _requests(mock)
    envelope = json.loads(request.content)
    assert envelope["method"] == "tools/call"
    assert envelope["params"] == {
        "name": "tako_search",
        "arguments": {"query": "q", "sources": ["data", "web"]},
    }
    assert result.backend == "tako:anonymous"
    (note,) = result.partial_failures
    assert "include_domains" in note["error"]
    assert "effort='deep'" in note["error"]


def test_anonymous_rate_limit_is_an_error_with_server_message(mock: respx.MockRouter) -> None:
    _route_mcp(mock, httpx.Response(200, json=_rpc_rate_limited()))

    with pytest.raises(
        RuntimeError, match="rate limited: Anonymous search is limited to 10 calls per minute"
    ):
        _search(_backend(api_key=None), query="a")


def test_anonymous_429_is_an_error(mock: respx.MockRouter) -> None:
    _route_mcp(
        mock,
        httpx.Response(
            429,
            json={
                "jsonrpc": "2.0",
                "id": None,
                "error": {"code": -32000, "message": "slow down", "data": {"kind": "rate_limited"}},
            },
        ),
    )

    with pytest.raises(RuntimeError, match="rate limited: slow down"):
        _search(_backend(api_key=None), query="a")


def test_anonymous_429_with_an_html_body_reports_the_page_text(mock: respx.MockRouter) -> None:
    _route_mcp(
        mock,
        httpx.Response(
            429,
            headers={"content-type": "text/html"},
            text="<html><body>429 Too Many Requests</body></html>",
        ),
    )

    with pytest.raises(RuntimeError, match="rate limited: .*429 Too Many Requests"):
        _search(_backend(api_key=None), query="a")


def test_anonymous_sse_reply_is_decoded(mock: respx.MockRouter) -> None:
    payload = json.dumps(_rpc_ok(ANONYMOUS_OUTPUT))
    _route_mcp(
        mock,
        httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            content=f"data: {payload}\n\n".encode(),
        ),
    )

    result = _search(_backend(api_key=None), query="a")

    assert result.backend == "tako:anonymous"
    assert len(result.sources) == 3


def _frame(seq: int, block: dict) -> str:
    envelope = {
        "seq": seq,
        "run_id": "run-1",
        "thread_id": "thr-1",
        "category": "activity",
        "block": block,
    }
    return f"data: {json.dumps(envelope)}\n\n"


def _sse(*frames: str) -> httpx.Response:
    return httpx.Response(
        200, headers={"content-type": "text/event-stream"}, content="".join(frames).encode()
    )


def _run_json(status: str, **fields) -> httpx.Response:
    return httpx.Response(
        200, json={"run_id": "run-1", "status": status, "created_at": "x", **fields}
    )


COMPLETED_RUN = _run_json("completed", result=ANSWER_RESULT, usage={"total_cost_usd": 0.11})

FULL_STREAM = (
    _frame(0, {"kind": "status", "message": "planning"}),
    _frame(
        1,
        {
            "kind": "tool_call",
            "id": "t1",
            "tool": "search_graph",
            "status_message": "looking up GDP",
        },
    ),
    _frame(
        2, {"kind": "subagent", "agent_id": "a1", "subagent_type": "retriever", "event": "dispatch"}
    ),
    _frame(3, {"kind": "heartbeat"}),
    _frame(4, {"kind": "agent_result", "id": "r1", "data": ANSWER_RESULT}),
    _frame(
        5,
        {
            "kind": "run_summary",
            "status": "completed",
            "created_at": "x",
            "usage": {"total_cost_usd": 0.49},
        },
    ),
    _frame(6, {"kind": "stream_done"}),
)


def _route_dispatch(mock: respx.MockRouter, response: httpx.Response):
    return mock.post(RUNS_URL).mock(return_value=response)


def _route_run(mock: respx.MockRouter, *responses: httpx.Response):
    return mock.get(host="tako.com", path=RUN_PATH).mock(side_effect=list(responses))


def test_deep_research_streams_progress_and_returns_the_result(mock: respx.MockRouter) -> None:
    _route_dispatch(mock, _sse(*FULL_STREAM))
    stages: list[str] = []

    result = _research(_backend(), stages.append, question="Why?")

    (request,) = _requests(mock)
    assert request.method == "POST"
    assert request.url == RUNS_URL
    assert request.headers["accept"] == "text/event-stream"
    assert request.headers["x-api-key"] == "TAKO_API_KEY"
    assert request.headers[CALLER_HEADER] == CALLER_VALUE
    body = json.loads(request.content)
    assert (body["query"], body["effort"]) == ("Why?", "medium")
    assert result.backend == "tako:agent"
    assert result.request_ids == ["run-1"]
    assert result.estimated_cost_usd == 0.49
    assert result.usage[0]["total_cost_usd"] == 0.49
    assert result.answer_markdown.startswith("GDP grew 2.1%")
    assert [d.source_id for d in result.sources] == [1, 2, 3]
    assert any("planning" in s for s in stages)
    assert any("calling search_graph: looking up GDP" in s for s in stages)
    assert any("dispatch retriever" in s for s in stages)


def test_deep_research_effort_high_and_processor_note(mock: respx.MockRouter) -> None:
    _route_dispatch(mock, _sse(*FULL_STREAM))

    result = _research(_backend(), question="Why?", effort="high", processor="pro-fast")

    assert json.loads(_requests(mock)[0].content)["effort"] == "high"
    assert any(
        "--processor" in f["error"] and "pro-fast" in f["error"] for f in result.partial_failures
    )


def test_a_non_sse_body_on_resume_falls_through_to_polling(
    mock: respx.MockRouter, no_backoff: None
) -> None:
    _route_dispatch(mock, _sse(FULL_STREAM[0], FULL_STREAM[1]))
    interstitial = httpx.Response(200, headers={"content-type": "text/html"}, text="<html>go away")
    _route_run(mock, interstitial, COMPLETED_RUN)
    stages: list[str] = []

    result = _research(_backend(), stages.append, question="Why?")

    assert [r.method for r in _requests(mock)] == ["POST", "GET", "GET"]
    assert _requests(mock)[1].url.params["starting_after"] == "1"
    assert _requests(mock)[2].headers["accept"] == "application/json"
    assert result.answer_markdown.startswith("GDP grew 2.1%")
    assert result.estimated_cost_usd == 0.11
    assert any("stream interrupted" in s for s in stages)


def test_a_stream_that_ends_without_a_result_polls_until_terminal(
    mock: respx.MockRouter, no_backoff: None
) -> None:
    _route_dispatch(
        mock,
        _sse(FULL_STREAM[0], FULL_STREAM[5], _frame(6, {"kind": "stream_done"})),
    )
    _route_run(mock, _run_json("running"), COMPLETED_RUN)
    stages: list[str] = []

    result = _research(_backend(), stages.append, question="Why?")

    assert [r.method for r in _requests(mock)] == ["POST", "GET", "GET"]
    assert any("state=running" in s for s in stages)
    assert result.estimated_cost_usd == 0.11


def test_an_agent_result_frame_the_sdk_cannot_read_falls_through_to_polling(
    mock: respx.MockRouter, no_backoff: None
) -> None:
    drifted_result = _frame(
        4,
        {
            "kind": "agent_result",
            "id": "r1",
            "data": {"answer": "a", "cards": [], "citations": [{"index": 1}]},
        },
    )
    _route_dispatch(mock, _sse(FULL_STREAM[0], drifted_result, FULL_STREAM[5], FULL_STREAM[6]))
    _route_run(mock, COMPLETED_RUN)

    result = _research(_backend(), question="Why?")

    assert [r.method for r in _requests(mock)] == ["POST", "GET"]
    assert result.answer_markdown.startswith("GDP grew 2.1%")


def test_deep_research_401_names_the_missing_grant(mock: respx.MockRouter) -> None:
    _route_dispatch(mock, httpx.Response(401, json={"error_message": "Invalid API key"}))

    with pytest.raises(RuntimeError, match="requires a valid, granted TAKO_API_KEY"):
        _research(_backend(), question="Why?")


def test_deep_research_without_key_is_an_error(mock: respx.MockRouter) -> None:
    with pytest.raises(RuntimeError, match="requires TAKO_API_KEY"):
        _research(_backend(api_key=None), question="Why?")

    assert _requests(mock) == []


def test_deep_research_refusal_is_an_error(mock: respx.MockRouter) -> None:
    refused = {**ANSWER_RESULT, "answer": None, "refusal_code": "rejected_input_classifier"}
    _route_dispatch(
        mock,
        _sse(
            _frame(0, {"kind": "agent_result", "id": "r", "data": refused}),
            _frame(1, {"kind": "run_summary", "status": "completed", "created_at": "x"}),
            _frame(2, {"kind": "stream_done"}),
        ),
    )

    with pytest.raises(RuntimeError, match="refusal_code=rejected_input_classifier"):
        _research(_backend(), question="Why?")


def test_deep_research_failed_run_is_an_error_without_polling(mock: respx.MockRouter) -> None:
    _route_dispatch(
        mock,
        _sse(
            _frame(
                0,
                {
                    "kind": "run_summary",
                    "status": "failed",
                    "created_at": "x",
                    "error": {"code": "boom", "message": "agent crashed"},
                },
            ),
            _frame(1, {"kind": "stream_done"}),
        ),
    )

    with pytest.raises(RuntimeError, match="run-1 failed: agent crashed"):
        _research(_backend(), question="Why?")

    assert [r.method for r in _requests(mock)] == ["POST"]


def test_a_zero_timeout_is_honored_and_the_message_mentions_no_cancel(
    mock: respx.MockRouter,
) -> None:
    _route_dispatch(mock, _sse(*FULL_STREAM))
    stages: list[str] = []

    with pytest.raises(RuntimeError, match="did not finish within 0s.*no cancel endpoint"):
        _research(_backend(), stages.append, question="Why?", timeout_seconds=0)

    assert any("timeout=0s" in s for s in stages)


TOOL_RETRY_FRAME = _frame(
    2,
    {
        "kind": "tool_retry",
        "id": "t2",
        "tool": "execute_dataframe_code",
        "parent_id": None,
        "elapsed_ms": 1200,
        "error": "Your output DataFrame is missing the year column.",
    },
)
OTHER_KIND_FRAMES = (
    _frame(1, {"kind": "reasoning", "id": "rs1", "delta": "thinking", "done": False}),
    _frame(
        1,
        {
            "kind": "tool_result",
            "id": "t1",
            "tool": "search",
            "parent_id": None,
            "elapsed_ms": 900,
            "link": None,
        },
    ),
    _frame(1, {"kind": "text", "id": "x1", "delta": "GDP grew", "done": False}),
    'data: {"seq": 1, "run_id": "run-1", "block": {"kind": 42}}\n\n',
)


def test_tool_retries_are_reported_and_other_kinds_do_not_break_the_stream(
    mock: respx.MockRouter,
) -> None:
    frames = (FULL_STREAM[0], TOOL_RETRY_FRAME, *OTHER_KIND_FRAMES, *FULL_STREAM[4:])
    _route_dispatch(mock, _sse(*frames))
    stages: list[str] = []

    result = _research(_backend(), stages.append, question="Why?")

    assert [r.method for r in _requests(mock)] == ["POST"]
    assert result.backend == "tako:agent"
    assert any(
        "retrying execute_dataframe_code: Your output DataFrame is missing the year column" in s
        for s in stages
    )
