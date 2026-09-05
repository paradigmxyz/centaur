# Websearch Plugin

Web search and deep research over one of two backends, with optional Claude
synthesis on top of `search` results.

| Backend (`WEBSEARCH_BACKEND`) | `search`, no key | `search`, keyed | `deep-research` (keyed only) |
| --- | --- | --- | --- |
| `tako` (default) | Anonymous `tako_search` on `mcp.tako.com`: Tako data cards plus web results | `POST https://tako.com/api/v3/search` with filters and `--effort` (`TAKO_API_KEY`) | Tako Answer Agent, `POST /api/v1/agent/answer/runs` (`TAKO_API_KEY`) |
| `parallel` | Parallel's free Search MCP | Parallel Search REST (`PARALLEL_API_KEY`) | Parallel Task API (`PARALLEL_API_KEY`) |

`ANTHROPIC_API_KEY` adds the Claude reviewer → writer → citation-repair
pipeline over whichever backend retrieved (`search` only). Without it, `search`
returns raw results and records the skipped synthesis in `meta.partial_failures`.

## How the backend is chosen

Two facts decide a call:

- **Vendor** comes from the `WEBSEARCH_BACKEND` environment variable (set it
  in the chart's `sandbox.extraEnv`). Unknown values fail at startup with the
  accepted values in the message.
- **Keyed or anonymous** comes from the principal's grant. The tool sends the
  injected placeholder to the vendor's REST endpoint; a 401 means the key isn't
  granted, and the tool falls back to that vendor's anonymous path for the rest
  of the process. Nothing probes the other vendor.

`meta.backend` reports which path served the call: `tako:api`,
`tako:anonymous`, `tako:agent`, `parallel:api`, `parallel:mcp`, or
`parallel:task:<processor>`.

Anonymous Tako search is rate limited per client IP (10 calls per minute) and
platform-wide. A limited call fails with the server's message. A real
deployment brings a `TAKO_API_KEY`.

On the Parallel free-MCP path the response carries a `meta.attribution` string
("Search powered by the free Parallel Web Search MCP …") that `--pretty`
surfaces. Retain or display it when you redistribute free-tier results. See
<https://parallel.ai/customer-terms>.

## Quickstart

```python
from websearch.client import WebSearchClient

client = WebSearchClient()
result = await client.search("US GDP growth since 2020")
# meta.backend is 'tako:anonymous' with no key, 'tako:api' with a granted TAKO_API_KEY
```

## Secrets

Set in root `.env` (preferred) or `tools/research/websearch/.env`.

- `TAKO_API_KEY` — optional. Unlocks Tako REST search (filters, `--effort deep`) and `deep-research` on the Answer Agent. Injected as `X-API-Key` on `tako.com` only.
- `PARALLEL_API_KEY` — optional, used when `WEBSEARCH_BACKEND=parallel`. Get one at <https://platform.parallel.ai>.
- `ANTHROPIC_API_KEY` — optional. Enables the Claude synthesis pipeline on `search`.

Non-secret config (synthesis model, base URLs, the default Parallel processor) is set with `WebSearchClient(...)` kwargs. Defaults: `synthesis_model="claude-opus-4-6"`, `tako_api_base_url="https://tako.com"`, `tako_mcp_url="https://mcp.tako.com/mcp"`, `parallel_api_base_url="https://api.parallel.ai"`, `parallel_mcp_url="https://search.parallel.ai/mcp"`, `parallel_deep_research_processor="ultra-fast"`.

## Tools

### `search`

```python
await client.search("How should a fintech startup evaluate MPC vs HSM in 2026?", num_results=10)
```

- `query: str` — required.
- `effort: "instant" | "fast" | "deep"` — default `fast`. On Tako, `deep` widens retrieval and adds a rerank at a higher price. On Parallel, `instant` maps to `basic` and the others to `advanced`. Anonymous paths record it in `meta.partial_failures`.
- `include_domains`, `exclude_domains: list[str]`, `max_age_hours: int` — keyed paths only. `max_age_hours` rounds down to a UTC calendar date on both vendors.
- `client_model`, `max_chars_total`, `session_id` — Parallel REST knobs. Tako ignores `client_model` and `session_id` and notes `max_chars_total` in `meta.partial_failures`.
- `synthesize: bool` — default `True`. Needs `ANTHROPIC_API_KEY`.

Results are `SourceDocument`s (`source_id`, `title`, `url`, `snippet`, `published_date`, `domain`). From Tako, data cards come first and web results follow. A card's `url` is its Tako page (the chart), its `snippet` is the data-bound description plus metric definitions and methodology, and its `domain` is the publisher (`International Monetary Fund`), not `tako.com`.

### `deep-research`

```python
await client.deep_research(
    "How should a fintech startup evaluate MPC vs HSM in 2026?", effort="high"
)
```

- `effort: "medium" | "high"` — default `medium`. Tako passes it to the Answer Agent. Parallel maps `medium` to `ultra-fast` and `high` to `ultra`.
- `timeout_seconds` — default 600 s on Tako, per-processor on Parallel. Neither vendor has a cancel endpoint: a run that outlives the budget keeps running and keeps costing.

On Tako, the report is the agent's answer with `[n]` citations, then `## Charts` (one link per card), `## Definitions`, `## Assumptions`, `## Methodology` (each only when present), and `## Sources`. `meta.estimated_cost_usd` is the run's actual billed cost from `usage.total_cost_usd`. A query Tako declines (`refusal_code`) fails with the code instead of returning an empty report.

On Parallel, the run goes through the Task API with auto schema and is restricted to the `pro`/`ultra` processor family (`lite`/`base`/`core` raise a clear error pointing at the docs).

#### Parallel processor cheatsheet

The hidden `--processor` flag overrides the `effort` mapping on Parallel only. Cost is per 1,000 runs.

| Processor       | Cost  | Latency band  | Use case |
| --------------- | -----:| -------------:| -------- |
| `pro-fast`      | $100  | 30s – 5min    | Quick research that still wants cross-source synthesis |
| `pro`           | $100  | 2min – 10min  | Same depth as `pro-fast`, less aggressive parallelism |
| `ultra-fast`    | $300  | 1min – 10min  | Default. Multi-source deep research with reasonable latency |
| `ultra`         | $300  | 5min – 25min  | Same depth, more time budget for harder questions |
| `ultra2x` … `ultra8x` | $600 – $2400 | 1min – 2hr | The most difficult deep research; rarely needed |

`-fast` variants are 2–5× faster than their non-fast siblings at the same price. See [Parallel pricing](https://docs.parallel.ai/getting-started/pricing) for the full table.

## CLI

```bash
# No credentials: anonymous Tako search
websearch search "Recent funding for AI search startups"

# With a granted TAKO_API_KEY: REST path with filters
websearch search "Recent funding for AI search startups" \
  --include-domain techcrunch.com --include-domain reuters.com \
  --max-age-hours 720 --effort deep --pretty

# Deep research on the Tako Answer Agent (requires TAKO_API_KEY)
websearch deep-research "comparison of L2 rollup economics" --effort high --pretty

# Parallel instead, for the whole deployment
WEBSEARCH_BACKEND=parallel websearch search "..." --pretty
```

### Backward-compatibility notes

- `--mode basic|advanced` still parses as a hidden flag and maps to `--effort instant|fast` with a deprecation note. Passing both is an error.
- `--processor` still parses as a hidden flag. On Parallel it overrides `--effort`; on Tako it's recorded in `meta.partial_failures`.
- Hidden flags `--search-type`, `--max-iterations`, `--num-queries-per-iteration`, `--num-results-per-query` are accepted, warn, and are ignored.
- `meta.exa_request_ids` mirrors `meta.request_ids`. `DeepResearchResponse.iterations` stays a single synthetic entry.

`meta.estimated_cost_usd`: on Tako REST it's the response's `usage.total_cost_usd` when present, else the rate table ($0.007 for `instant`/`fast`, $0.012 for `deep`); `0.0` on anonymous paths; the run's actual cost for the Answer Agent. On Parallel it's an estimate from list prices.
