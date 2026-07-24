"""Pure selection + formatting helpers for the `available-data` command.

Python port of tako-mcp's `_available_data.ts`. `available-data` resolves a
name to graph node(s) and, for each, reports what data Tako has for it — as a
natural-language coverage summary. Everything here is network-free so it can
be unit-tested in isolation; `client._run_available_data` orchestrates the
fetches and calls these.

Coverage is type-aware, because the graph models the two node kinds
differently:
  - an ENTITY node (Tesla) → its `metrics` group is the data Tako holds.
  - a METRIC node (Inflation Rate) has no `metrics`; its coverage is the
    `entities` group — the entities the metric is tracked across.
Drilling `relation=metrics` on a metric node returns empty, so reporting
"no metrics" there would be a false negative — hence the split.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from dataclasses import asdict, dataclass
from typing import Any

# How many search hits get a coverage drill. Two keeps disambiguation
# ("Tesla, Inc." vs another "Tesla") visible while staying cheap.
EXPAND_TOP_N = 2
# Cap for the coverage name list, matched to the graph/related drill's fetch
# limit. Coverage names are the primary payload — each is a term the agent
# reuses in a follow-up search — so completeness beats brevity. `total` /
# `truncated` still report when even more exist server-side.
PREVIEW = 50
OTHER_MATCH_PREVIEW = 5

# Metric names that read as internal/accounting plumbing rather than the
# headline figures a person expects first. Deprioritized in the preview only —
# they still count toward `total`. Kept deliberately narrow so a real metric
# is never hidden, only pushed down.
LOW_SIGNAL_METRIC = re.compile(r"\(Normalized\)|^Account Code\b", re.IGNORECASE)


def enum_value(value: Any) -> Any:
    """Unwrap a str-Enum (SDK GraphNodeType / NerLabel) to its value."""
    return getattr(value, "value", value)


def coverage_kind_for(node_type: Any) -> str:
    """The relation to drill (and coverage kind it yields) for a node type."""
    return "entities" if enum_value(node_type) == "metric" else "metrics"


@dataclass(frozen=True)
class CoverageGroup:
    """One coverage group (metrics of an entity, or entities of a metric)."""

    kind: str
    names: tuple[str, ...]
    total: int
    truncated: bool
    capped: bool


@dataclass(frozen=True)
class CoverageMatch:
    """Coverage for one expanded node."""

    node_id: str
    name: str
    type: str
    label: str | None
    coverage: CoverageGroup
    unavailable: bool = False


@dataclass(frozen=True)
class OtherMatch:
    """A search hit beyond EXPAND_TOP_N — named but not drilled."""

    name: str
    type: str


def _empty_group(kind: str) -> CoverageGroup:
    return CoverageGroup(kind=kind, names=(), total=0, truncated=False, capped=False)


def order_metric_names(names: Sequence[str]) -> list[str]:
    """Stable partition: headline metric names first, low-signal ones after.

    Only reorders — never drops a name.
    """
    clean = [n for n in names if not LOW_SIGNAL_METRIC.search(n)]
    noisy = [n for n in names if LOW_SIGNAL_METRIC.search(n)]
    return clean + noisy


def select_coverage(page: Any, kind: str) -> CoverageGroup:
    """Build a CoverageGroup from a drilled GraphRelationPage (or None).

    Metrics are reordered headline-first; entities keep the backend's order.
    Caps at PREVIEW and reports the server total + capped flag.
    """
    if page is None:
        return _empty_group(kind)
    raw = [item.name for item in (page.items or [])]
    ordered = order_metric_names(raw) if kind == "metrics" else list(raw)
    total = page.total if page.total is not None else len(ordered)
    names = tuple(ordered[:PREVIEW])
    # Capped means the server stopped counting — more names always exist
    # beyond the floor, even if `total` happens to equal the shown count.
    capped = bool(page.total_capped)
    return CoverageGroup(
        kind=kind,
        names=names,
        total=total,
        truncated=capped or total > len(names),
        capped=capped,
    )


def build_match(node: Any, page: Any) -> CoverageMatch:
    """A CoverageMatch from a resolved GraphNode + its drilled coverage page."""
    kind = coverage_kind_for(node.type)
    return CoverageMatch(
        node_id=node.id,
        name=node.name,
        type=enum_value(node.type),
        label=enum_value(node.label),
        coverage=select_coverage(page, kind),
    )


def unavailable_match(node: Any) -> CoverageMatch:
    """A match whose coverage lookup failed — resolved, coverage unavailable."""
    return CoverageMatch(
        node_id=node.id,
        name=node.name,
        type=enum_value(node.type),
        label=enum_value(node.label),
        coverage=_empty_group(coverage_kind_for(node.type)),
        unavailable=True,
    )


def has_live_coverage(match: CoverageMatch) -> bool:
    """Real, loaded coverage — resolved AND drill succeeded AND non-empty.

    Drives both the summary header and the `found` flag, so "found" always
    means "Tako has data", never merely "a node matched".
    """
    return not match.unavailable and match.coverage.total > 0


def match_to_dict(match: CoverageMatch) -> dict:
    """JSON-shaped dict for CLI output (tuples become lists)."""
    data = asdict(match)
    data["coverage"]["names"] = list(match.coverage.names)
    return data
