# Tako research tool — design

**Date:** 2026-07-24
**Status:** approved pending user review
**Location:** `tools/research/tako/`

## Goal

Finish the `tako` tool plugin under `tools/research/tako/` and ship it as a PR.
The scaffold (client, CLI, pyproject, README, .env.example) already exists and
its SDK usage is verified against `tako-sdk` 2.2.6. This design replaces the
three raw knowledge-graph commands with a single `available-data` command,
ported from the `tako_available_data` tool in the user's Tako MCP
(`tako-mcp/workers/src/tools/tako_available_data.ts` + `_available_data.ts`).

## Why combine graph-search + graph-related into available-data

The graph drill is type-aware: an **entity** node's coverage is its `metrics`
relation; a **metric** node's coverage is the `entities` relation (the entities
it is tracked across). Drilling `relation=metrics` on a metric node returns
empty — a false "no data" answer. With independent commands every agent must
re-derive that branching from help text each session; the MCP encodes it once
in the tool, and that design is proven there. The combined call also collapses
two round trips (resolve → drill) into one free call whose output is either
"yes, and here are the exact names to search" or "no".

What's given up (pagination cursors, `q`-filtering within a relation, cohort
walking, siblings) is rare for this tool's job. The `total`/`truncated`/
`capped` fields still report when more exists server-side. `graph_search` and
`graph_related` stay as private methods on `TakoClient` so re-exposing them
later is a five-line CLI change.

## Verified API references (tako-sdk 2.2.6, inspected from the wheel)

- `tako.lib.Tako(config)` facade: `search(SearchRequest)`,
  `answer(SearchRequest)`, `contents(ContentsRequest)`,
  `graph_search(q, types, limit, label, infer_label)`,
  `graph_related(node_id, relation, relation_type, q, cursor, limit, label,
  infer_label)`, `graph_node(node_id)`.
- `GraphSearchResponse.results: list[GraphNode]`;
  `GraphNode = {id, type: "entity"|"metric", name, aliases, description,
  subtype, label: NerLabel|None}`.
- `graph_related(relation=...)` fills `GraphRelatedResponse.relation:
  GraphRelationPage = {key, kind, label, items: list[GraphNode], total,
  total_capped, next_cursor}`. Page size default 50, max 100.
- `NerLabel` enum: PERSON, ORG, GPE, LOC, PRODUCT, EVENT, LANGUAGE, MONEY,
  METRIC, STOCK_TICKER, WEBSITE — identical to the MCP's label list.
- Auth: `config.api_key["apiKey"]` → `X-API-Key` header → host `tako.com`
  (matches `[tool.centaur]` secret binding already in pyproject).
- Transport is urllib3: proxy + CA bundle must be wired explicitly
  (`Configuration.proxy` / `ssl_ca_cert`) or sandbox requests bypass
  iron-proxy and get dropped. Already handled in the scaffold; keep.

## Surface

CLI commands (each `--help` string is the model-facing tool description):

| Command | Cost | Purpose |
| --- | --- | --- |
| `health` | free | Auth/connectivity probe via `_graph_search("nvidia", limit=1)` |
| `available-data` | free | "What data does Tako have on X?" — run FIRST |
| `search` | priced | Structured data cards + web results |
| `answer` | priced | Synthesized written answer + supporting cards |
| `contents` | priced beyond free allowance | Underlying data behind a result URL |

Removed: `graph-search`, `graph-related`, `graph-node` CLI commands and the
public `graph_node` client method (per user: node tool not needed).

Kept: `--node-id` (repeatable) + `--strict` on `search`/`answer` — the MCP's
`tako_search` keeps `node_ids` ("from tako_available_data, or a card's
nodes"; pinned nodes get a strong retrieval boost; `strict` is a hard filter).
`available-data` output includes `node_id` per match to feed them.

## `available_data(q, types=None, label=None)` — pipeline

Port of the MCP pipeline, sync (the drill is at most 2 sequential calls; not
worth going async):

1. `_graph_search(q, types=types, limit=10, label=label)`. `types` is
   `"entity" | "metric" | None`; `label` one of the NER labels (boost, not
   filter).
2. No results → `{found: false, ...}` with a "no node matched; try search
   directly or rephrase" summary. No drill calls.
3. Top `EXPAND_TOP_N = 2` hits each get a coverage drill:
   `_graph_related(node.id, relation=("entities" if node.type == "metric"
   else "metrics"), limit=PREVIEW)` with `PREVIEW = 50`. Per-node error
   isolation: a failed or malformed drill yields an `unavailable` match, never
   sinks the call. Hits 3–10 become `other_matches: [{name, type}]`.
4. Deterministic output (no LLM call):

```json
{
  "found": true,
  "query": "tesla",
  "summary": "Tako's proprietary data has live, continuously-updated coverage of ...",
  "matches": [
    {
      "node_id": "tesla-inc-abc123",
      "name": "Tesla, Inc.",
      "type": "entity",
      "label": "ORG",
      "coverage": {
        "kind": "metrics",
        "names": ["Revenue", "..."],
        "total": 120,
        "truncated": true,
        "capped": false
      }
    }
  ],
  "other_matches": [{ "name": "Tesla Energy", "type": "entity" }]
}
```

Semantics ported exactly from `_available_data.ts`:

- `found` is true only when some match has live coverage (`!unavailable &&
  total > 0`) — node resolution alone is never "data".
- Metric-name preview is reordered headline-first: names matching
  `\(Normalized\)|^Account Code\b` (case-insensitive) are pushed to the end of
  the preview, never dropped, and still count toward `total`.
- Capped totals render as `N+` in the summary; `truncated = capped or
  total > len(names)`.
- Summary is built deterministically: header (claims coverage only for
  matches that have it), one line per match, `Also matched: ...` tail for
  other_matches (preview 5), and a next-step example composed entity+metric
  (e.g. `"Tesla, Inc. Revenue"`) only when a match has real coverage.
- Coverage names appear once, in `matches[].coverage.names` — the summary
  references but does not repeat them.

## Files

```text
tools/research/tako/
├── __init__.py       NEW — one-line docstring (websearch convention)
├── client.py         EDIT — add available_data(); graph_search/graph_related
│                     become _graph_search/_graph_related; delete graph_node;
│                     search/answer/contents unchanged
├── _coverage.py      NEW — pure port of _available_data.ts: constants
│                     (EXPAND_TOP_N, PREVIEW, OTHER_MATCH_PREVIEW,
│                     LOW_SIGNAL_METRIC), coverage_kind_for, order_metric_names,
│                     select_coverage, build_match, unavailable_match,
│                     has_live_coverage, build_summary. Frozen dataclasses,
│                     no I/O — unit-testable in isolation
├── cli.py            EDIT — drop graph-search/graph-related/graph-node; add
│                     available-data (args: q; options: --types, --label);
│                     health switches to client._graph_search
├── test_client.py    NEW — unit tests for _coverage.py (no network):
│                     kind branching, empty/unavailable matches, capped/
│                     truncated flags, metric reordering, summary phrasing,
│                     found semantics, next-step example selection
├── README.md         EDIT — rewrite around the available-data-first flow;
│                     keep cost table, maintainer notes (proxy, timeouts)
├── pyproject.toml    UNCHANGED
└── .env.example      UNCHANGED
```

`_coverage.py` is `_`-prefixed to signal helper-not-tool, mirroring the MCP's
`_available_data.ts` convention.

## Error handling

- `available_data`: graph search failure → raise with a clear message
  (auth/connectivity — nothing to salvage). Drill failure → per-node
  `unavailable` match (auth already proven by the search).
- CLI: existing pattern — exceptions surface as non-zero exit with the error
  printed; `health` wraps in the `{ok, tool, error, details}` envelope.
- Input validation: `q` min length 2 (matches MCP); `--types` restricted to
  entity|metric; `--label` restricted to the NerLabel values.

## How the model chooses this tool

The CLI help text teaches the flow (same language as the MCP description):

1. `tako available-data "Tesla"` — free; run first to confirm coverage and get
   exact names before any priced call. Entity → its metrics; metric → the
   entities tracked.
2. `tako search "Tesla, Inc. Revenue" --node-id tesla-inc-abc123` — priced;
   reuse coverage names verbatim, optionally pin node ids from step 1.
3. `tako answer "..."` — priced; when prose synthesis is wanted, not cards.
4. `tako contents <url>` — pull the full dataset behind a card
   (`--quote-only` to price it first).

Versus `websearch`: Tako is for numbers, trends, and cohort comparisons from
licensed structured sources; websearch is for narrative and recency. The
README states this split so an orchestrating agent routes correctly.

## Testing

- Unit tests for every pure function in `_coverage.py` (pytest, no network).
- `client.py` / `cli.py` verified by import + `--help` smoke run locally;
  `tako health` + one `available-data` call run manually if a TAKO_API_KEY is
  available at review time.

## Out of scope

- The SDK agent products (`client.agent.*`) — async run/poll/stream fits a
  workflow, not a one-shot CLI (already noted in scaffold README).
- `create_card`, graph pagination, cohort walking, per-request timeouts
  (SDK facade doesn't forward `_request_timeout`; noted for upstream).
