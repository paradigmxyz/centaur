# Websearch Plugin

Web search and deep research via [Parallel Web Systems](https://docs.parallel.ai),
with optional Claude synthesis on top of search results.

| Capability                       | No credentials                                  | + `PARALLEL_API_KEY`                                  | + `ANTHROPIC_API_KEY`                                 |
| -------------------------------- | ----------------------------------------------- | ----------------------------------------------------- | ----------------------------------------------------- |
| `search` (sources + excerpts)    | Free hosted MCP, lower rate limits              | Parallel Search REST, higher limits + filters         | (same; synthesis layered on top — see below)          |
| `search(synthesize=True)`        | Raw excerpts only, skipped synthesis flagged    | Raw excerpts only, skipped synthesis flagged          | Claude reviewer → writer → citation-repair pipeline   |
| `deep_research`                  | not available (key required)                    | Parallel Task API (default processor `ultra-fast`)    | (same)                                                |

Each credential is additive: drop in only what you need.

The free MCP path is provided by Parallel; on that path the response includes a `meta.attribution` string ("Search powered by the free Parallel Web Search MCP …"). The CLI `--pretty` mode surfaces it. Please retain or display this attribution when redistributing free-tier results. See <https://parallel.ai/customer-terms>.

## Quickstart

```python
from websearch.client import WebSearchClient

client = WebSearchClient()
result = await client.search("Parallel Web Systems funding")
# meta.backend will be 'parallel:mcp' (no key) or 'parallel:api' (with key)
```

That's the minimum — works with zero credentials via the free MCP. Add a `PARALLEL_API_KEY` to use the paid REST and unlock `deep_research`; add `ANTHROPIC_API_KEY` to get cited markdown synthesis on top of the results.

## Secrets

Set in root `.env` (preferred) or `tools/research/websearch/.env`.

- `PARALLEL_API_KEY` — optional. Get one at <https://platform.parallel.ai>. Unlocks the paid Search REST path (with source filters) and enables `deep_research` via the Task API.
- `ANTHROPIC_API_KEY` — optional. Enables the Claude-backed synthesis pipeline on `search(synthesize=True)`.

Non-secret config (synthesis model, default Task processor, REST/MCP base URLs) is configured via `WebSearchClient(...)` constructor kwargs at instantiation time. Defaults: `synthesis_model="claude-opus-4-6"`, `parallel_deep_research_processor="ultra-fast"`, `parallel_api_base_url="https://api.parallel.ai"`, `parallel_mcp_url="https://search.parallel.ai/mcp"`.

## Tools

### `search`

```python
await client.search(
    "How should a fintech startup evaluate MPC vs HSM in 2026?",
    num_results=10,
)
```

Arguments:

- `query: str` — **required** positional. The user's question or topic. Forwarded to Parallel as both the `objective` and the sole `search_queries` entry.
- `client_model: str` — identifier of the LLM consuming the excerpts (e.g. `claude-opus-4-7`). Enables per-model retrieval/excerpt tuning.
- `session_id: str` — stable UUID reused across related Search/Extract calls; used for free-tier rate limiting on the MCP.

REST-only (silently warned via `meta.partial_failures` when used over the free MCP):

- `mode: "basic" | "advanced"` — default `advanced` on REST. MCP forces `basic`.
- `max_chars_total: int` — hard cap on total excerpt characters.
- `include_domains`, `exclude_domains: list[str]` — domain filters.
- `max_age_hours: int` — recency filter, rounded **down to a UTC calendar-date** cutoff (Parallel's `source_policy.after_date` is date-granular, not hour-precise — `max_age_hours=6` becomes "published on or after today's date").

Set `synthesize=True` (default) to also run the Claude reviewer → writer → citation-repair pipeline against the retrieved sources. Without `ANTHROPIC_API_KEY` the call still returns raw Parallel sources and records the skipped synthesis in `meta.partial_failures`.

### `deep_research`

```python
await client.deep_research(
    "How should a fintech startup evaluate MPC vs HSM in 2026?",
    processor="pro-fast",  # optional
)
```

Creates a Parallel Task API run with auto schema, polls to completion, and renders the structured JSON output as cited markdown.

**Requires `PARALLEL_API_KEY`** — the free MCP does not include deep-research. Restricted to the `pro/ultra` processor family (`lite`/`base`/`core` raise a clear error pointing at the docs).

### Processor cheatsheet

Pick a `processor` based on the depth and latency you need (cost is per 1 000 runs):

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
# Works with zero credentials (free MCP)
ai-v2 tools run websearch search "Recent funding for AI search startups"

# With PARALLEL_API_KEY: REST path + filters
PARALLEL_API_KEY=... ai-v2 tools run websearch search \
  "Recent funding for AI search startups" \
  --include-domain techcrunch.com --include-domain reuters.com \
  --max-age-hours 720 --pretty

# Deep research (requires PARALLEL_API_KEY)
PARALLEL_API_KEY=... ai-v2 tools run websearch deep-research \
  "How should a fintech startup evaluate MPC vs HSM in 2026?" \
  --processor pro-fast --pretty
```

### Backward-compatibility notes

- Hidden CLI flags `--search-type`, `--max-iterations`, `--num-queries-per-iteration`, `--num-results-per-query` are accepted for prod-shaped invocations but emit a deprecation notice and are ignored under Parallel retrieval.
- `meta.exa_request_ids` is retained as an alias of `meta.request_ids` so existing consumers of the original Exa-backed response shape still work.
- `DeepResearchResponse.iterations` is retained as a single-element list per call (Parallel Task API is single-call rather than iterative).

`meta.backend` in the JSON payload reports `parallel:mcp`, `parallel:api`, or `parallel:task:<processor>` so you can confirm which path served the call.

`meta.estimated_cost_usd` is a best-effort estimate from Parallel's published list prices ([pricing](https://docs.parallel.ai/getting-started/pricing)) — `0.0` on the free MCP path, `$0.005`+ per REST search, and the per-run processor price for `deep_research`. The API does not return billed cost, so treat this as an estimate that can drift if prices change.
