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


from tools.research.tako.client import _run_available_data


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
        others = [OtherMatch(name=f"Match {i}", type="entity") for i in range(7)]
        matches = [build_match(_node(), _page(["Revenue"]))]
        summary = build_summary("tesla", matches, others)
        assert (
            "Also matched: Match 0, Match 1, Match 2, Match 3, Match 4, and 2 more."
            in summary
        )

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
            {"name": "Tesla 2", "type": "entity"},
            {"name": "Tesla 3", "type": "entity"},
            {"name": "Tesla 4", "type": "entity"},
        ]

    def test_output_is_json_serializable(self):
        import json

        fake = FakeGraph(
            search_results=[_node(node_id="e1", label="ORG")],
            related_by_node={"e1": _page(["Revenue"])},
        )
        json.dumps(_run(fake))
