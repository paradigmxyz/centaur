"""Pure selection and formatting helpers for the `available-data` command.

Python port of tako-mcp's `_available_data.ts`. `available-data` resolves a
name to graph nodes and, for each, reports what data Tako has for it as a
natural-language coverage summary. Everything here is network-free so it can
be unit-tested in isolation; `client._run_available_data` orchestrates the
fetches and calls these.

Coverage is type-aware, because the graph models the two node kinds
differently. An ENTITY node (Tesla) holds its data in the `metrics` group.
A METRIC node (Inflation Rate) has no `metrics`; its coverage is the
`entities` group, the entities the metric is tracked across. Drilling
`relation=metrics` on a metric node returns empty, so reporting "no metrics"
there would be a false negative. That is why the drill splits by type.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from dataclasses import asdict, dataclass
from typing import Any

from tako.models.graph_node_type import GraphNodeType
from tako.models.ner_label import NerLabel

# How many search hits get a coverage drill. Two keeps disambiguation
# ("Tesla, Inc." vs another "Tesla") visible while staying cheap.
EXPAND_TOP_N = 2
# Cap for the coverage name list, matched to the graph/related drill's fetch
# limit. Coverage names are the primary payload (each is a term the agent
# reuses in a follow-up search), so completeness beats brevity. `total` and
# `truncated` still report when even more exist server-side.
PREVIEW = 50
OTHER_MATCH_PREVIEW = 5

# Metric names that read as internal accounting plumbing rather than the
# headline figures a person expects first. Deprioritized in the preview only;
# they still count toward `total`. Kept deliberately narrow so a real metric
# is never hidden, only pushed down.
LOW_SIGNAL_METRIC = re.compile(r"\(Normalized\)|^Account Code\b", re.IGNORECASE)

# The installed CLI command name (the [project.scripts] entry). Referenced in
# model-facing summary strings so a rename stays consistent everywhere.
TOOL_COMMAND = "datasearch"

# Valid values, derived from the SDK enums so an upstream addition is accepted
# here the moment the dependency updates (the pin is open: tako-sdk>=2.2.6).
# The API, not this tool, is the authority on what these accept.
NODE_TYPES = tuple(t.value for t in GraphNodeType)
# A label is a ranking boost, not a filter.
NER_LABELS = tuple(label.value for label in NerLabel)


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
    """A search hit beyond EXPAND_TOP_N: resolved but NOT coverage-checked.

    Carries node_id so an agent that recognizes the right entity here can pin
    it in a follow-up `search --node-id` without a second resolve.
    """

    node_id: str
    name: str
    type: str


def _empty_group(kind: str) -> CoverageGroup:
    return CoverageGroup(kind=kind, names=(), total=0, truncated=False, capped=False)


def order_metric_names(names: Sequence[str]) -> list[str]:
    """Stable partition: headline metric names first, low-signal ones after.

    Only reorders; it never drops a name.
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
    # Capped means the server stopped counting: more names always exist
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
    """A CoverageMatch from a resolved GraphNode and its drilled coverage page."""
    kind = coverage_kind_for(node.type)
    return CoverageMatch(
        node_id=node.id,
        name=node.name,
        type=enum_value(node.type),
        label=enum_value(node.label),
        coverage=select_coverage(page, kind),
    )


def unavailable_match(node: Any) -> CoverageMatch:
    """A match that resolved but whose coverage lookup failed."""
    return CoverageMatch(
        node_id=node.id,
        name=node.name,
        type=enum_value(node.type),
        label=enum_value(node.label),
        coverage=_empty_group(coverage_kind_for(node.type)),
        unavailable=True,
    )


def has_live_coverage(match: CoverageMatch) -> bool:
    """Real, loaded coverage: resolved, drill succeeded, and non-empty.

    Drives both the summary header and the `found` flag, so "found" always
    means "Tako has data", never merely "a node matched".
    """
    return not match.unavailable and match.coverage.total > 0


def match_to_dict(match: CoverageMatch) -> dict:
    """JSON-shaped dict for CLI output (tuples become lists)."""
    data = asdict(match)
    data["coverage"]["names"] = list(match.coverage.names)
    return data


def _plural(n: int, one: str, many: str) -> str:
    return one if n == 1 else many


# Counts only. The names themselves live once, in matches[].coverage.names;
# the whole output object is what the model reads, so repeating names here
# would double their token cost. Capped totals render as "N+" (a floor).
def _count_str(group: CoverageGroup) -> str:
    return f"{group.total}{'+' if group.capped else ''}"


def _label_suffix(match: CoverageMatch) -> str:
    return f" ({match.label})" if match.label else ""


def _coverage_clause(group: CoverageGroup) -> str:
    if group.kind == "entities":
        return f"tracked for {_count_str(group)} {_plural(group.total, 'entity', 'entities')}"
    return f"{_count_str(group)} {_plural(group.total, 'metric', 'metrics')}"


def _empty_clause(kind: str) -> str:
    if kind == "entities":
        return "resolved, but Tako isn't tracking it against any entities yet"
    return "resolved, but Tako holds no metrics for it yet"


def _match_line(match: CoverageMatch) -> str:
    head = f"**{match.name}{_label_suffix(match)}**"
    if match.unavailable:
        return f"{head} — resolved, but Tako couldn't load its coverage right now (temporary); retry."
    if match.coverage.total == 0:
        return f"{head} — {_empty_clause(match.coverage.kind)}."
    return f"{head} — {_coverage_clause(match.coverage)}."


def _next_step_example(matches: Sequence[CoverageMatch]) -> str | None:
    """A real follow-up query, composed entity-plus-metric.

    For an entity's metrics that reads "Tesla, Inc. Revenue"; for a metric's
    entities, "United States Inflation Rate". Only matches with actual
    coverage qualify. There is deliberately no fallback: it would steer the
    agent into a priced search for a name it just reported as having no data.
    """
    for match in matches:
        if not match.unavailable and match.coverage.names:
            first = match.coverage.names[0]
            if match.coverage.kind == "entities":
                return f"{first} {match.name}"
            return f"{match.name} {first}"
    return None


def build_summary(
    query: str,
    matches: Sequence[CoverageMatch],
    other_matches: Sequence[OtherMatch],
) -> str:
    """The natural-language coverage summary, the narrative shell of the
    `available-data` output. Coverage names are not repeated here; they live
    once, in matches[].coverage.names. Node ids never appear here.
    """
    if not matches:
        return (
            f'Tako has no data-graph node matching "{query}". Tako may still have '
            f"relevant public/web data — try `{TOOL_COMMAND} search` directly, or rephrase "
            "the entity or metric name."
        )

    n = len(matches)
    # The header only claims coverage for matches that actually have some; a
    # resolved node with no coverage (or a failed drill) must not be
    # advertised as data. "Tako's proprietary data" is the grammatical subject
    # on purpose: downstream models echo this header nearly verbatim.
    with_data = sum(1 for m in matches if has_live_coverage(m))
    matches_of = f'{n} {_plural(n, "match", "matches")} for "{query}"'
    covers = "Tako's proprietary data has live, continuously-updated coverage of"
    if with_data == 0:
        header = f"Resolved {matches_of}, but none with live data coverage:"
    elif with_data < n:
        header = f"{covers} {with_data} of {matches_of}:"
    else:
        header = f"{covers} {matches_of}:"

    blocks = [header, "", "\n\n".join(_match_line(m) for m in matches)]

    if other_matches:
        names = [o.name for o in other_matches[:OTHER_MATCH_PREVIEW]]
        rest = len(other_matches) - len(names)
        tail = f", and {rest} more" if rest > 0 else ""
        blocks.extend(["", f"Also matched (not checked): {', '.join(names)}{tail}."])
        # Coverage was only drilled for the top matches. Without this note, a
        # zero-coverage result reads as "Tako has no data" when the right
        # entity may simply have ranked below the drill cutoff.
        if with_data == 0:
            blocks.extend(
                [
                    "",
                    f"Only the top {n} {_plural(n, 'match was', 'matches were')} "
                    "coverage-checked; the \"Also matched\" hits were not, so this "
                    "is not proof Tako lacks data for them. If one of them is the "
                    "intended entity or metric, rerun with `--types`/`--label` to "
                    "narrow resolution, or pin its node_id directly in "
                    f"`{TOOL_COMMAND} search --node-id <id>`.",
                ]
            )

    example = _next_step_example(matches)
    if example:
        blocks.extend(
            [
                "",
                "The exact names are listed in each match's coverage.names. To pull "
                "one as a chart or dataset, run "
                f"`{TOOL_COMMAND} search` with entity + metric "
                f'(e.g. "{example}").',
            ]
        )

    return "\n".join(blocks)
