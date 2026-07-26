"""Unit tests for the tako tool's pure coverage helpers and pipeline. No network."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tako.models.graph_node import GraphNode
from tako.models.graph_node_type import GraphNodeType
from tako.models.graph_related_response import GraphRelatedResponse
from tako.models.graph_relation_page import GraphRelationPage
from tako.models.graph_search_response import GraphSearchResponse

from tools.research.tako._coverage import (
    PREVIEW,
    CoverageGroup,
    CoverageMatch,
    OtherMatch,
    build_match,
    build_summary,
    coverage_kind_for,
    enum_value,
    has_live_coverage,
    match_to_dict,
    order_metric_names,
    select_coverage,
    unavailable_match,
)


from tools.research.tako.client import TakoClient, _run_available_data, _sources


def _node(node_id="tesla-inc-abc", node_type="entity", name="Tesla, Inc.", label=None):
    return GraphNode(id=node_id, type=node_type, name=name, label=label)


def _page(names, total=None, total_capped=False, kind="data", key="metrics"):
    items = [_node(node_id=f"n-{i}", name=n) for i, n in enumerate(names)]
    return GraphRelationPage(
        key=key,
        kind=kind,
        label="Metrics",
        items=items,
        total=total if total is not None else len(items),
        total_capped=total_capped,
    )


class TestEnumValue:
    def test_unwraps_str_enum(self):
        assert enum_value(GraphNodeType.METRIC) == "metric"

    def test_passes_plain_string_through(self):
        assert enum_value("entity") == "entity"

    def test_passes_none_through(self):
        assert enum_value(None) is None


class TestCoverageKindFor:
    def test_entity_node_drills_metrics(self):
        assert coverage_kind_for("entity") == "metrics"

    def test_metric_node_drills_entities(self):
        assert coverage_kind_for("metric") == "entities"

    def test_accepts_sdk_enum(self):
        assert coverage_kind_for(GraphNodeType.METRIC) == "entities"


class TestOrderMetricNames:
    def test_pushes_low_signal_names_to_end_keeping_relative_order(self):
        names = [
            "Account Code - Inventory Valuation (Normalized)",
            "Revenue",
            "Gross Profit (Normalized)",
            "Net Income",
        ]
        assert order_metric_names(names) == [
            "Revenue",
            "Net Income",
            "Account Code - Inventory Valuation (Normalized)",
            "Gross Profit (Normalized)",
        ]

    def test_never_drops_names(self):
        names = ["Account Code - X", "(Normalized)"]
        assert sorted(order_metric_names(names)) == sorted(names)

    def test_does_not_mutate_input(self):
        names = ["Revenue (Normalized)", "Revenue"]
        order_metric_names(names)
        assert names == ["Revenue (Normalized)", "Revenue"]


class TestSelectCoverage:
    def test_none_page_yields_empty_group(self):
        group = select_coverage(None, "metrics")
        assert group == CoverageGroup(
            kind="metrics", names=(), total=0, truncated=False, capped=False
        )

    def test_names_and_total_from_page(self):
        group = select_coverage(_page(["Revenue", "Net Income"]), "metrics")
        assert group.names == ("Revenue", "Net Income")
        assert group.total == 2
        assert group.truncated is False
        assert group.capped is False

    def test_truncated_when_server_total_exceeds_names(self):
        group = select_coverage(_page(["Revenue"], total=120), "metrics")
        assert group.total == 120
        assert group.truncated is True

    def test_capped_forces_truncated_even_when_counts_match(self):
        group = select_coverage(_page(["Revenue"], total=1, total_capped=True), "metrics")
        assert group.capped is True
        assert group.truncated is True

    def test_metrics_kind_reorders_low_signal_names(self):
        group = select_coverage(_page(["Revenue (Normalized)", "Revenue"]), "metrics")
        assert group.names == ("Revenue", "Revenue (Normalized)")

    def test_entities_kind_keeps_backend_order(self):
        group = select_coverage(
            _page(["Zimbabwe (Normalized)", "United States"], key="entities"), "entities"
        )
        assert group.names == ("Zimbabwe (Normalized)", "United States")

    def test_preview_cap(self):
        group = select_coverage(_page([f"Metric {i}" for i in range(60)]), "metrics")
        assert len(group.names) == PREVIEW
        assert group.truncated is True


class TestMatches:
    def test_build_match_for_entity(self):
        match = build_match(_node(label="ORG"), _page(["Revenue"]))
        assert match.node_id == "tesla-inc-abc"
        assert match.name == "Tesla, Inc."
        assert match.type == "entity"
        assert match.label == "ORG"
        assert match.unavailable is False
        assert match.coverage.kind == "metrics"
        assert match.coverage.names == ("Revenue",)

    def test_build_match_for_metric_uses_entities_kind(self):
        node = _node(node_type="metric", name="Inflation Rate")
        match = build_match(node, _page(["United States"], key="entities"))
        assert match.coverage.kind == "entities"

    def test_unavailable_match(self):
        match = unavailable_match(_node())
        assert match.unavailable is True
        assert match.coverage.total == 0

    def test_has_live_coverage_requires_total_and_availability(self):
        live = build_match(_node(), _page(["Revenue"]))
        resolved_no_data = build_match(_node(), _page([]))
        failed = unavailable_match(_node())
        assert has_live_coverage(live) is True
        assert has_live_coverage(resolved_no_data) is False
        assert has_live_coverage(failed) is False

    def test_match_to_dict_is_json_shaped(self):
        match = build_match(_node(label="ORG"), _page(["Revenue"]))
        data = match_to_dict(match)
        assert data["node_id"] == "tesla-inc-abc"
        assert data["coverage"]["names"] == ["Revenue"]
        assert isinstance(data["coverage"]["names"], list)


class TestBuildSummary:
    def test_no_matches_points_at_direct_search(self):
        summary = build_summary("xyzzy", [], [])
        assert 'no data-graph node matching "xyzzy"' in summary
        assert "tako search" in summary

    def test_full_coverage_header(self):
        matches = [build_match(_node(label="ORG"), _page(["Revenue"], total=120))]
        summary = build_summary("tesla", matches, [])
        assert (
            "Tako's proprietary data has live, continuously-updated coverage of "
            '1 match for "tesla":' in summary
        )
        assert "**Tesla, Inc. (ORG)** — 120 metrics." in summary

    def test_partial_coverage_header_counts_only_live_matches(self):
        live = build_match(_node(), _page(["Revenue"]))
        empty = build_match(_node(name="Tesla Energy"), _page([]))
        summary = build_summary("tesla", [live, empty], [])
        assert '2 matches for "tesla"' in summary
        assert "coverage of 1 of 2 matches" in summary
        assert "resolved, but Tako holds no metrics for it yet" in summary

    def test_zero_coverage_header(self):
        empty = build_match(_node(), _page([]))
        summary = build_summary("tesla", [empty], [])
        assert summary.startswith(
            'Resolved 1 match for "tesla", but none with live data coverage:'
        )

    def test_metric_match_phrasing(self):
        node = _node(node_type="metric", name="Inflation Rate")
        match = build_match(node, _page(["United States"], total=63, key="entities"))
        summary = build_summary("inflation", [match], [])
        assert "tracked for 63 entities." in summary

    def test_capped_total_renders_plus(self):
        match = build_match(_node(), _page(["Revenue"], total=50, total_capped=True))
        summary = build_summary("tesla", [match], [])
        assert "50+ metrics." in summary

    def test_unavailable_phrasing(self):
        summary = build_summary("tesla", [unavailable_match(_node())], [])
        assert "couldn't load its coverage right now (temporary); retry." in summary

    def test_other_matches_tail_with_overflow(self):
        others = [OtherMatch(node_id=f"n{i}", name=f"Match {i}", type="entity") for i in range(7)]
        matches = [build_match(_node(), _page(["Revenue"]))]
        summary = build_summary("tesla", matches, others)
        assert (
            "Also matched (not checked): Match 0, Match 1, Match 2, Match 3, Match 4, "
            "and 2 more." in summary
        )

    def test_zero_coverage_with_other_matches_flags_unchecked_hits(self):
        others = [OtherMatch(node_id="n1", name="Tesla, Inc.", type="entity")]
        matches = [build_match(_node(name="Tesla Energy"), _page([]))]
        summary = build_summary("tesla", matches, others)
        assert "coverage-checked" in summary
        assert "not proof Tako lacks data" in summary

    def test_live_coverage_does_not_flag_unchecked_hits(self):
        others = [OtherMatch(node_id="n1", name="Tesla Energy", type="entity")]
        matches = [build_match(_node(), _page(["Revenue"]))]
        summary = build_summary("tesla", matches, others)
        assert "not proof Tako lacks data" not in summary

    def test_next_step_example_entity_composes_name_then_metric(self):
        match = build_match(_node(), _page(["Revenue"]))
        summary = build_summary("tesla", [match], [])
        assert '(e.g. "Tesla, Inc. Revenue")' in summary

    def test_next_step_example_metric_composes_entity_then_name(self):
        node = _node(node_type="metric", name="Inflation Rate")
        match = build_match(node, _page(["United States"], key="entities"))
        summary = build_summary("inflation", [match], [])
        assert '(e.g. "United States Inflation Rate")' in summary

    def test_no_next_step_example_without_coverage(self):
        summary = build_summary("tesla", [build_match(_node(), _page([]))], [])
        assert "e.g." not in summary


class FakeGraph:
    """Stand-in for TakoClient._graph_search/_graph_related."""

    def __init__(self, search_results, related_by_node=None, related_error_for=()):
        self._search = GraphSearchResponse(results=search_results)
        self._related_by_node = related_by_node or {}
        self._related_error_for = set(related_error_for)
        self.related_calls = []

    def graph_search(self, q, types=None, limit=None, label=None):
        self.last_search = {"q": q, "types": types, "limit": limit, "label": label}
        return self._search

    def graph_related(self, node_id, relation=None, limit=None):
        self.related_calls.append({"node_id": node_id, "relation": relation, "limit": limit})
        if node_id in self._related_error_for:
            raise RuntimeError("boom")
        return GraphRelatedResponse(
            node=_node(node_id=node_id), relation=self._related_by_node.get(node_id)
        )


def _run(fake, q="tesla", **kwargs):
    return _run_available_data(
        q, graph_search=fake.graph_search, graph_related=fake.graph_related, **kwargs
    )


class TestRunAvailableData:
    def test_no_results_short_circuits_without_drilling(self):
        fake = FakeGraph(search_results=[])
        result = _run(fake, q="xyzzy")
        assert result["found"] is False
        assert result["matches"] == []
        assert result["other_matches"] == []
        assert fake.related_calls == []
        assert 'no data-graph node matching "xyzzy"' in result["summary"]

    def test_search_uses_limit_10_and_forwards_filters(self):
        fake = FakeGraph(search_results=[])
        _run(fake, types="entity", label="ORG")
        assert fake.last_search == {"q": "tesla", "types": "entity", "limit": 10, "label": "ORG"}

    def test_entity_node_drills_metrics_and_metric_node_drills_entities(self):
        entity = _node(node_id="e1")
        metric = _node(node_id="m1", node_type="metric", name="Inflation Rate")
        fake = FakeGraph(
            search_results=[entity, metric],
            related_by_node={
                "e1": _page(["Revenue"]),
                "m1": _page(["United States"], key="entities"),
            },
        )
        result = _run(fake)
        assert [c["relation"] for c in fake.related_calls] == ["metrics", "entities"]
        assert all(c["limit"] == PREVIEW for c in fake.related_calls)
        assert result["found"] is True
        assert result["matches"][0]["coverage"]["kind"] == "metrics"
        assert result["matches"][1]["coverage"]["kind"] == "entities"

    def test_drill_failure_isolated_to_unavailable_match(self):
        fake = FakeGraph(
            search_results=[_node(node_id="e1"), _node(node_id="e2", name="Tesla Energy")],
            related_by_node={"e2": _page(["Revenue"])},
            related_error_for=("e1",),
        )
        result = _run(fake)
        assert result["matches"][0]["unavailable"] is True
        assert result["matches"][1]["unavailable"] is False
        assert result["found"] is True

    def test_found_false_when_resolution_has_no_coverage(self):
        fake = FakeGraph(
            search_results=[_node(node_id="e1")],
            related_by_node={"e1": _page([])},
        )
        assert _run(fake)["found"] is False

    def test_hits_beyond_top_n_become_other_matches(self):
        nodes = [_node(node_id=f"e{i}", name=f"Tesla {i}") for i in range(5)]
        fake = FakeGraph(
            search_results=nodes,
            related_by_node={"e0": _page(["Revenue"]), "e1": _page(["Revenue"])},
        )
        result = _run(fake)
        assert len(fake.related_calls) == 2
        assert result["other_matches"] == [
            {"node_id": "e2", "name": "Tesla 2", "type": "entity"},
            {"node_id": "e3", "name": "Tesla 3", "type": "entity"},
            {"node_id": "e4", "name": "Tesla 4", "type": "entity"},
        ]

    def test_output_is_json_serializable(self):
        import json

        fake = FakeGraph(
            search_results=[_node(node_id="e1", label="ORG")],
            related_by_node={"e1": _page(["Revenue"])},
        )
        json.dumps(_run(fake))


class TestRunAvailableDataValidation:
    def test_short_q_raises(self):
        fake = FakeGraph(search_results=[])
        try:
            _run(fake, q="x")
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "at least 2 characters" in str(exc)

    def test_bad_types_raises(self):
        fake = FakeGraph(search_results=[])
        try:
            _run(fake, types="cohort")
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "entity" in str(exc) and "metric" in str(exc)

    def test_bad_label_raises(self):
        fake = FakeGraph(search_results=[])
        try:
            _run(fake, label="COMPANY")
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "ORG" in str(exc)

    def test_validation_happens_before_any_network_call(self):
        fake = FakeGraph(search_results=[])
        try:
            _run(fake, q="x")
        except ValueError:
            pass
        assert not hasattr(fake, "last_search")


class TestSources:
    def test_all_defaults_returns_none_for_api_default(self):
        assert _sources(None, None, None, False) is None

    def test_data_count_zero_gives_web_only(self):
        sources = _sources(0, None, None, False)
        assert sources.data is None
        assert sources.web is not None

    def test_web_count_zero_gives_data_only(self):
        sources = _sources(None, 0, None, False)
        assert sources.web is None
        assert sources.data is not None

    def test_counts_and_node_ids_forwarded(self):
        sources = _sources(5, 3, ["n1", "n2"], True)
        assert sources.data.count == 5
        assert sources.data.node_ids == ["n1", "n2"]
        assert sources.data.strict is True
        assert sources.web.count == 3

    def test_strict_without_node_ids_raises(self):
        try:
            _sources(None, None, None, True)
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "strict" in str(exc)

    def test_more_than_max_node_ids_raises(self):
        try:
            _sources(None, None, [f"n{i}" for i in range(21)], False)
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "20" in str(exc)


class _FakeSdkResponse:
    def __init__(self, payload):
        self._payload = payload

    def model_dump(self, mode=None, exclude_none=None):
        return self._payload


class FakeTakoApi:
    """Stand-in for the generated TakoApi; records requests and timeouts."""

    def __init__(self):
        self.calls = []

    def search(self, request, _request_timeout=None):
        self.calls.append(("search", request, _request_timeout))
        return _FakeSdkResponse({"cards": []})

    def answer(self, request, _request_timeout=None):
        self.calls.append(("answer", request, _request_timeout))
        return _FakeSdkResponse({"answer": "", "cards": []})

    def contents(self, request, _request_timeout=None):
        self.calls.append(("contents", request, _request_timeout))
        return _FakeSdkResponse({"outputs": []})

    def graph_search(self, q, types=None, limit=None, label=None, infer_label=None,
                     _request_timeout=None):
        self.calls.append(("graph_search", q, _request_timeout))
        return GraphSearchResponse(results=[])


class FakeTakoFacade:
    def __init__(self):
        self._api = FakeTakoApi()


def _client_with_fake():
    client = TakoClient.__new__(TakoClient)
    client._client = FakeTakoFacade()
    client._timeout = 120.0
    client._graph_timeout = 30.0
    client._has_key = True
    client._mcp = None
    return client


class TestPricedPaths:
    def test_search_builds_request_and_dumps(self):
        client = _client_with_fake()
        result = client.search(
            "tesla revenue", effort="deep", data_count=2, node_ids=["n1"], strict=True
        )
        method, request, timeout = client._client._api.calls[0]
        assert method == "search"
        assert request.query == "tesla revenue"
        assert request.effort == "deep"
        assert request.sources.data.node_ids == ["n1"]
        assert timeout == 120.0
        assert result["cards"] == []
        assert result["meta"] == {"backend": "tako:sdk", "partial_failures": []}

    def test_answer_builds_same_request_shape(self):
        client = _client_with_fake()
        client.answer("us cpi since 2020", web_count=0)
        method, request, timeout = client._client._api.calls[0]
        assert method == "answer"
        assert request.query == "us cpi since 2020"
        assert request.sources.web is None
        assert timeout == 120.0

    def test_search_strict_without_node_ids_raises_before_network(self):
        client = _client_with_fake()
        try:
            client.search("tesla", strict=True)
            raise AssertionError("expected ValueError")
        except ValueError:
            pass
        assert client._client._api.calls == []

    def test_search_invalid_effort_raises_before_network(self):
        client = _client_with_fake()
        try:
            client.search("tesla", effort="medium")
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "effort" in str(exc) and "deep" in str(exc)
        assert client._client._api.calls == []

    def test_contents_builds_request(self):
        client = _client_with_fake()
        result = client.contents(
            "https://tako.com/card/abc", mode="inline", content_format="csv", max_rows=100
        )
        method, request, timeout = client._client._api.calls[0]
        assert method == "contents"
        assert request.url == "https://tako.com/card/abc"
        assert request.mode == "inline"
        assert request.content_format == "csv"
        assert request.max_rows == 100
        assert request.quote_only is None
        assert timeout == 120.0
        assert result["outputs"] == []
        assert result["meta"]["backend"] == "tako:sdk"

    def test_contents_invalid_mode_and_format_raise_before_network(self):
        client = _client_with_fake()
        for kwargs in ({"mode": "attachment"}, {"content_format": "xml"}):
            try:
                client.contents("https://tako.com/card/abc", **kwargs)
                raise AssertionError("expected ValueError")
            except ValueError:
                pass
        assert client._client._api.calls == []

    def test_graph_search_forwards_graph_timeout(self):
        client = _client_with_fake()
        client._graph_search("nvidia", limit=1)
        method, q, timeout = client._client._api.calls[0]
        assert method == "graph_search"
        assert timeout == 30.0


class TestDrillFailureLogging:
    def test_drill_failure_emits_warning(self, caplog):
        import logging

        fake = FakeGraph(search_results=[_node(node_id="e1")], related_error_for=("e1",))
        with caplog.at_level(logging.WARNING, logger="tools.research.tako.client"):
            _run(fake)
        assert any("coverage drill failed" in r.message for r in caplog.records)


class TestSourcesReviewFindings:
    def test_skipped_source_is_absent_from_wire_body_not_null(self):
        sources = _sources(0, None, None, False)
        wire = sources.to_dict()
        assert "data" not in wire
        assert "data" not in sources.model_fields_set
        assert "web" in wire

    def test_default_source_is_also_absent_when_only_other_is_set(self):
        wire = _sources(None, 0, None, False).to_dict()
        assert "web" not in wire
        assert "data" in wire

    def test_data_count_zero_with_node_ids_raises(self):
        try:
            _sources(0, None, ["n1"], False)
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "contradicts" in str(exc)

    def test_data_count_zero_with_strict_raises(self):
        try:
            _sources(0, None, ["n1"], True)
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "contradicts" in str(exc)

    def test_negative_counts_rejected_not_folded_into_skip(self):
        for kwargs in ((-3, None), (None, -1)):
            try:
                _sources(kwargs[0], kwargs[1], None, False)
                raise AssertionError("expected ValueError")
            except ValueError as exc:
                assert "between 0" in str(exc)

    def test_over_limit_counts_rejected(self):
        try:
            _sources(50, None, None, False)
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "20" in str(exc)

    def test_skipping_both_sources_rejected(self):
        try:
            _sources(0, 0, None, False)
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "both sources" in str(exc)


class TestDerivedConstants:
    def test_ner_labels_track_the_sdk_enum(self):
        from tako.models.ner_label import NerLabel

        from tools.research.tako._coverage import NER_LABELS

        assert set(NER_LABELS) == {label.value for label in NerLabel}

    def test_node_types_track_the_sdk_enum(self):
        from tools.research.tako._coverage import NODE_TYPES

        assert set(NODE_TYPES) == {t.value for t in GraphNodeType}

    def test_effort_and_contents_options_track_the_sdk_enums(self):
        from tako.models.contents_delivery_mode import ContentsDeliveryMode
        from tako.models.contents_format import ContentsFormat
        from tako.models.search_effort_level import SearchEffortLevel

        from tools.research.tako.client import (
            CONTENT_FORMATS,
            CONTENT_MODES,
            EFFORT_LEVELS,
        )

        assert set(EFFORT_LEVELS) == {e.value for e in SearchEffortLevel}
        assert set(CONTENT_MODES) == {m.value for m in ContentsDeliveryMode}
        assert set(CONTENT_FORMATS) == {f.value for f in ContentsFormat}


# -- MCP fallback backend ----------------------------------------------------

import httpx  # noqa: E402

from tools.research.tako._mcp import (  # noqa: E402
    McpAuthRequired,
    TakoMcpBackend,
    _sources_and_count,
)


def _mcp_transport(tool_payloads, status_code=200, record=None):
    """MockTransport speaking just enough Streamable HTTP for the backend.

    tool_payloads: dict of tool name -> structuredContent payload.
    record: optional list collecting (method, params) JSON-RPC pairs.
    """

    def handler(request: httpx.Request) -> httpx.Response:
        if status_code != 200:
            return httpx.Response(status_code, json={"detail": "denied"})
        body = json.loads(request.content)
        if record is not None:
            record.append((body.get("method"), body.get("params")))
        method = body.get("method")
        if method == "initialize":
            return httpx.Response(
                200, json={"jsonrpc": "2.0", "id": body["id"], "result": {}}
            )
        if method == "notifications/initialized":
            return httpx.Response(202)
        if method == "tools/call":
            name = body["params"]["name"]
            return httpx.Response(
                200,
                json={
                    "jsonrpc": "2.0",
                    "id": body["id"],
                    "result": {"structuredContent": tool_payloads[name]},
                },
            )
        raise AssertionError(f"unexpected method {method}")

    return httpx.MockTransport(handler)


import json  # noqa: E402


class TestMcpBackend:
    def test_search_maps_args_and_strips_widget_fields(self):
        record = []
        payload = {
            "cards": [{"title": "Revenue"}],
            "web_results": [],
            "usage": None,
            "request_id": "r1",
            "pub_id": "widget",
            "embed_url": "https://x",
        }
        backend = TakoMcpBackend(
            transport=_mcp_transport({"tako_search": payload}, record=record)
        )
        result = backend.search("tesla revenue", effort="deep", web_count=0)
        assert result["cards"] == [{"title": "Revenue"}]
        assert "pub_id" not in result and "embed_url" not in result
        assert result["meta"]["backend"] == "tako:mcp"
        call = next(p for m, p in record if m == "tools/call")
        assert call["name"] == "tako_search"
        assert call["arguments"]["sources"] == ["data"]
        assert call["arguments"]["effort"] == "deep"

    def test_differing_counts_recorded_as_partial_failure(self):
        sources, count, failures = _sources_and_count(5, 3)
        assert sources is None
        assert count == 5
        assert failures and failures[0]["feature"] == "per_source_counts"

    def test_answer_flags_unsupported_effort(self):
        payload = {"answer": "text", "cards": [], "request_id": "r2"}
        backend = TakoMcpBackend(transport=_mcp_transport({"tako_answer": payload}))
        result = backend.answer("us cpi", effort="deep")
        features = [f["feature"] for f in result["meta"]["partial_failures"]]
        assert "effort" in features

    def test_available_data_passes_through(self):
        payload = {
            "found": True,
            "query": "tesla",
            "summary": "s",
            "matches": [],
            "other_matches": [],
        }
        backend = TakoMcpBackend(
            transport=_mcp_transport({"tako_available_data": payload})
        )
        result = backend.available_data("tesla", types="entity")
        assert result["found"] is True
        assert result["meta"]["backend"] == "tako:mcp"

    def test_auth_rejection_is_a_clear_message(self):
        backend = TakoMcpBackend(transport=_mcp_transport({}, status_code=401))
        try:
            backend.available_data("tesla")
            raise AssertionError("expected McpAuthRequired")
        except McpAuthRequired as exc:
            assert "TAKO_API_KEY" in str(exc)


class TestBackendRouting:
    def _keyless_client(self, tool_payloads):
        client = TakoClient.__new__(TakoClient)
        client._has_key = False
        client._client = None
        client._timeout = 120.0
        client._graph_timeout = 30.0
        client._mcp = TakoMcpBackend(transport=_mcp_transport(tool_payloads))
        return client

    def test_keyless_search_routes_to_mcp(self):
        payload = {"cards": [], "web_results": [], "usage": None, "request_id": "r"}
        client = self._keyless_client({"tako_search": payload})
        result = client.search("tesla")
        assert result["meta"]["backend"] == "tako:mcp"

    def test_keyless_search_still_validates_contracts(self):
        client = self._keyless_client({})
        try:
            client.search("tesla", strict=True)
            raise AssertionError("expected ValueError")
        except ValueError:
            pass

    def test_keyless_contents_requires_key(self):
        client = self._keyless_client({})
        try:
            client.contents("https://tako.com/card/abc")
            raise AssertionError("expected ValueError")
        except ValueError as exc:
            assert "TAKO_API_KEY" in str(exc)

    def test_keyless_available_data_validates_then_routes(self):
        payload = {"found": False, "query": "q", "summary": "s", "matches": [], "other_matches": []}
        client = self._keyless_client({"tako_available_data": payload})
        try:
            client.available_data("x")
            raise AssertionError("expected ValueError")
        except ValueError:
            pass
        result = client.available_data("nvidia")
        assert result["meta"]["backend"] == "tako:mcp"

    def test_sdk_path_stamps_meta(self):
        client = _client_with_fake()
        result = client.search("tesla revenue")
        assert result["meta"]["backend"] == "tako:sdk"
