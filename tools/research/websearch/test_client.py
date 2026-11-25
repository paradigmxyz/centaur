from __future__ import annotations

import json
import os
import subprocess
import sys

import httpx
import pytest
from centaur_tool_websearch import cli
from centaur_tool_websearch.client import WebSearchClient
from typer.testing import CliRunner

from centaur_sdk.backends import StubBackend, configure


@pytest.fixture(autouse=True)
def stub_secrets() -> None:
    configure(StubBackend())


def _recording_transport(routes: dict[str, httpx.Response], requests: list[httpx.Request]):
    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return routes[str(request.url)]

    return httpx.MockTransport(handler)


def test_parallel_falls_back_to_anonymous_free_search_when_key_is_rejected() -> None:
    requests: list[httpx.Request] = []
    transport = _recording_transport(
        {
            "https://api.parallel.ai/v1/search": httpx.Response(401, json={"message": "bad key"}),
            "https://search.parallel.ai/mcp": httpx.Response(
                200,
                headers={"content-type": "text/event-stream"},
                text="event: message\ndata: "
                + json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "structuredContent": {
                                "search_id": "search_1",
                                "results": [
                                    {
                                        "url": "https://a.example",
                                        "title": "A",
                                        "publish_date": "2026-01-02",
                                        "excerpts": ["one", "two"],
                                    }
                                ],
                            }
                        },
                    }
                )
                + "\n\n",
            ),
        },
        requests,
    )

    payload = WebSearchClient(transport=transport).search(
        "rust async", include_domains=["docs.rs"], session_id="s1"
    )

    assert requests[0].headers["x-api-key"] == "PARALLEL_API_KEY"
    assert "x-api-key" not in requests[1].headers
    assert json.loads(requests[1].content)["params"]["arguments"]["session_id"] == "s1"
    assert payload["results"] == [
        {
            "title": "A",
            "url": "https://a.example",
            "snippet": "one\n\ntwo",
            "published_date": "2026-01-02",
        }
    ]
    assert payload["meta"]["backend"] == "parallel:free"
    assert "include_domains" in payload["meta"]["warnings"][0]
    assert "parallel.ai" in payload["meta"]["attribution"]


def test_parallel_api_receives_filters() -> None:
    requests: list[httpx.Request] = []
    transport = _recording_transport(
        {
            "https://api.parallel.ai/v1/search": httpx.Response(
                200, json={"search_id": "search_2", "results": []}
            )
        },
        requests,
    )

    payload = WebSearchClient(transport=transport).search(
        "rust async",
        num_results=5,
        include_domains=["docs.rs"],
        exclude_domains=["reddit.com"],
        max_age_hours=48,
        mode="basic",
        max_chars_total=1000,
        session_id="s1",
    )

    body = json.loads(requests[0].content)
    assert body["mode"] == "basic"
    assert body["max_chars_total"] == 1000
    assert body["session_id"] == "s1"
    assert body["advanced_settings"]["max_results"] == 5
    policy = body["advanced_settings"]["source_policy"]
    assert policy["include_domains"] == ["docs.rs"]
    assert policy["exclude_domains"] == ["reddit.com"]
    assert len(policy["after_date"]) == len("2026-01-01")
    assert payload["meta"] == {"backend": "parallel:api", "request_id": "search_2", "warnings": []}


def test_exa_maps_flags_and_caps_total_snippet_characters() -> None:
    requests: list[httpx.Request] = []
    transport = _recording_transport(
        {
            "https://api.exa.ai/search": httpx.Response(
                200,
                json={
                    "requestId": "req_1",
                    "results": [
                        {"url": "https://a.example", "title": "A", "highlights": ["abcdef"]},
                        {"url": "https://a.example", "title": "A again", "text": "dup"},
                        {"url": "https://b.example", "title": None, "text": "ghijkl"},
                        {"url": "https://c.example", "title": "C", "text": "mnop"},
                    ],
                },
            )
        },
        requests,
    )

    payload = WebSearchClient(transport=transport).search(
        "rust async",
        backend="exa",
        mode="advanced",
        max_age_hours=1,
        max_chars_total=8,
        session_id="s1",
    )

    body = json.loads(requests[0].content)
    assert requests[0].headers["x-api-key"] == "EXA_API_KEY"
    assert body["type"] == "auto"
    assert body["startPublishedDate"].endswith("+00:00")
    assert [(r["title"], r["snippet"]) for r in payload["results"]] == [
        ("A", "abcdef"),
        ("https://b.example", "gh"),
        ("C", ""),
    ]
    assert payload["meta"]["warnings"] == ["Exa ignores session_id."]


def test_tako_returns_data_cards_then_web_results(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("WEBSEARCH_BACKEND", "tako")
    requests: list[httpx.Request] = []
    transport = _recording_transport(
        {
            "https://tako.com/api/v3/search": httpx.Response(
                200,
                json={
                    "request_id": "req_2",
                    "cards": [
                        {
                            "title": "GDP",
                            "description": "US GDP",
                            "webpage_url": "https://tako.com/card/1",
                            "data_freshness": {"last_updated": "2026-03-01"},
                        },
                        {"title": "No URL"},
                    ],
                    "web_results": [{"title": "News", "url": "https://n.example", "snippet": "s"}],
                },
            )
        },
        requests,
    )

    payload = WebSearchClient(transport=transport).search(
        "us gdp", mode="basic", exclude_domains=["x.com"]
    )

    body = json.loads(requests[0].content)
    assert requests[0].headers["x-api-key"] == "TAKO_API_KEY"
    assert body["effort"] == "instant"
    assert body["sources"]["web"]["exclude_domains"] == ["x.com"]
    assert payload["results"] == [
        {
            "title": "GDP",
            "url": "https://tako.com/card/1",
            "snippet": "US GDP",
            "published_date": "2026-03-01",
        },
        {"title": "News", "url": "https://n.example", "snippet": "s", "published_date": None},
    ]


def test_keyed_backend_reports_rejected_key() -> None:
    transport = httpx.MockTransport(lambda _request: httpx.Response(403))

    with pytest.raises(RuntimeError, match="TAKO_API_KEY was rejected"):
        WebSearchClient(transport=transport).search("us gdp", backend="tako")


def _empty_parallel_transport() -> httpx.MockTransport:
    return httpx.MockTransport(
        lambda _request: httpx.Response(200, json={"search_id": "search_3", "results": []})
    )


def test_retired_arguments_are_ignored_with_a_warning() -> None:
    payload = WebSearchClient(transport=_empty_parallel_transport()).search(
        "rust async", synthesize=False, thread_context=["earlier"], client_model=None
    )

    assert payload["meta"]["warnings"] == [
        "websearch ignores unsupported arguments: synthesize, thread_context."
    ]


def test_cli_accepts_retired_flags(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        cli, "_client", lambda: WebSearchClient(transport=_empty_parallel_transport())
    )

    result = CliRunner().invoke(
        cli.app,
        [
            "search",
            "rust async",
            "--no-synthesize",
            "--client-model",
            "m",
            "--max-report-chars",
            "5",
        ],
    )

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["meta"]["warnings"] == [
        "websearch ignores unsupported arguments: client_model, max_report_chars, synthesize."
    ]


def test_cli_backend_flag_overrides_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    requests: list[httpx.Request] = []
    transport = _recording_transport(
        {"https://api.exa.ai/search": httpx.Response(200, json={"results": []})}, requests
    )
    monkeypatch.setenv("WEBSEARCH_BACKEND", "tako")
    monkeypatch.setattr(cli, "_client", lambda: WebSearchClient(transport=transport))

    result = CliRunner().invoke(cli.app, ["search", "rust async", "--backend", "exa"])

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["meta"]["backend"] == "exa"
    assert [str(request.url) for request in requests] == ["https://api.exa.ai/search"]


def test_unknown_backend_is_rejected() -> None:
    with pytest.raises(ValueError, match="parallel, exa, tako"):
        WebSearchClient().search("rust async", backend="bing")


def test_cli_exits_zero_when_reader_closes_pipe() -> None:
    script = """
import sys
from centaur_tool_websearch import client

def fake_search(self, query, **_kwargs):
    result = {"title": "t", "url": "https://x.example", "snippet": "s" * 1000, "published_date": None}
    return {"query": query, "results": [result] * 1000, "meta": {"backend": "fake", "warnings": []}}

client.WebSearchClient.search = fake_search
from centaur_tool_websearch.cli import app
sys.argv = ["websearch", "search", "q"]
app()
"""
    process = subprocess.Popen(
        [sys.executable, "-c", script],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=os.environ,
    )
    assert process.stdout is not None
    process.stdout.read(1)
    process.stdout.close()

    assert process.wait(timeout=30) == 0, process.stderr.read().decode()
