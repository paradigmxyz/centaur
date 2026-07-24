"""Unit tests for the tako tool's pure coverage helpers and pipeline. No network."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tako.models.graph_node import GraphNode
from tako.models.graph_node_type import GraphNodeType
from tako.models.graph_relation_page import GraphRelationPage

from tools.research.tako._coverage import (
    PREVIEW,
    CoverageGroup,
    CoverageMatch,
    build_match,
    coverage_kind_for,
    enum_value,
    has_live_coverage,
    match_to_dict,
    order_metric_names,
    select_coverage,
    unavailable_match,
)


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
