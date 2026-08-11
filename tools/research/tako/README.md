# datasearch (Tako)

Web search plus structured, cited data from [Tako](https://tako.com) across
companies & financial markets, macroeconomics & government data, digital &
industry intelligence, sports, polling & live events, and weather & climate,
with synthesized answers and free data-coverage discovery. Installed as the
`datasearch` CLI; the package and directory keep the `tako` name.

Coverage is described by domain rather than by the licensed sources behind it:
sources change without the coverage changing, and each card carries its own
attribution. `available-data` is the authority on whether a specific entity or
metric is covered, and it is free. The domain list has one definition,
`DOMAINS` in `_coverage.py`, which the CLI help renders from.

Where `websearch` retrieves prose from web pages, Tako returns typed series and
comparable metrics with sources attached. The two complement each other. Use
`websearch` for narrative and recency, and reach for `tako` when the answer is
a number, a trend, or a cohort comparison.

## Setup

| Capability | No credentials (free MCP tier) | + `TAKO_API_KEY` |
| --- | --- | --- |
| `available-data` | Free hosted MCP, rate-limited; `other_matches` carry name/type only | Direct graph API; `other_matches` include `node_id` |
| `search` | Free hosted MCP, rate-limited, single shared result count, no `--effort deep` | Full API: per-source counts, higher limits, `deep` |
| `answer` | Free hosted MCP, rate-limited, no `--effort` | Full API |
| `contents` | not available (key required) | Row exports, 20-row free allowance |
| `health` | Probes the free MCP | Probes the graph API |

Backend defaults differ when no count options are passed: the hosted MCP
returns up to 10 results per source, each card with a small inline row
preview (the free tier's only data access, since `contents` is key-only),
while the API defaults to 5 pointer cards per source, with `contents`
available for full rows. Every knob a backend cannot honor is reported in
`meta.partial_failures`, never silently dropped.

The credential is additive, like websearch's `PARALLEL_API_KEY`: with no key
configured, `search`/`answer`/`available-data` fall back to Tako's anonymous
free MCP tier at `mcp.tako.com` (TakoData/tako-mcp#171). The tier is enabled
per server environment, fail-closed: where it is off, keyless calls report
that a key is required. Where it is on, `tools/call` is metered at 10/min
per client IP; sandboxes NATed through one egress IP share that bucket, so
treat the free tier as a fallback, not fleet capacity. Over-limit calls
raise a clear rate-limit error carrying the server's own message and
retry hint. Responses carry `meta.backend` (`tako:sdk` or `tako:mcp`) and
`meta.partial_failures` for knobs the free tier cannot honor.

Both backends return the same shape. `search` returns `answer_markdown` (a
readable `## Tako Data` document the model synthesizes from) alongside
structured `cards` (with `image_url`, `webpage_url`, `node_ids`, `exportable`,
and row `content`); `answer` returns `answer` prose plus supporting `cards`.
The SDK path renders `answer_markdown` from its cards; the MCP path takes it
from the hosted tool's text channel and reads the structured fields from
`structuredContent`. Card rows are pointed at, not inlined, matching the MCP
(TakoData/tako-mcp#187 moved bulk payload into `structuredContent`); on
free-tier servers before that change, `cards` may be empty while
`answer_markdown` still carries the full readable document.

Inside a sandbox the routing works differently, because tool secrets leave no
env signal there: `secret()` returns a placeholder and iron-proxy swaps it on
the wire, so the tool cannot see whether the deployment vault holds the key.
When the sandbox firewall is active (`HTTPS_PROXY` set), the client takes the
SDK path optimistically and falls back to the free MCP tier once, on the
first auth rejection, remembering the switch for the client's lifetime. A
deployment with the key never notices; a deployment without one pays a
single failed probe and then runs keyless.

```bash
TAKO_API_KEY=...   # optional; https://tako.com, account settings
```

## The flow: discover free, then search priced

```bash
# 1. Free: does Tako have data on this, and what is it called?
datasearch available-data "Tesla"

# 2. Priced: pull the card, reusing the exact name (and optionally the node id)
datasearch search "Tesla, Inc. Revenue" --node-id <node_id from step 1>

# 3. Priced beyond the free allowance: export the full dataset behind a card
datasearch contents "<card webpage_url>" --content-format json_records
```

`available-data` is the recommended first step for any data lookup. It costs
nothing, it confirms coverage before you spend a priced call, and the exact
metric and entity names it returns let the follow-up land precisely instead of
guessing. Skip it only for an obvious open-web query that no data graph would
hold.

It is type-aware. An **entity** ("Tesla") reports the metrics Tako tracks for
it; a **metric** ("Inflation Rate") reports the entities it is tracked across.
One metric across many entities is a single metric-first call, and one entity
across many metrics is a single entity-first call. Never loop one call per
name.

In the output, `found` is true only when a match has live coverage; node
resolution alone never counts. `coverage.names` lists the exact names to reuse
verbatim, and `total`/`truncated`/`capped` report when more exist server-side.
`capped` means the server stopped counting, so read the total as "at least N".

Only the top two matches are coverage-checked. Hits beyond them appear in
`other_matches`, explicitly marked "not checked" in the summary: `found:
false` means "not confirmed in the checked set", never "Tako has no data".
On the keyed backend each entry carries its `node_id`; the hosted MCP returns
name/type only today. If an unchecked hit is the intended entity, pin its
node id in `search --node-id`, or rerun with `--types`/`--label` to rank it
higher.

## Commands

```bash
datasearch health
datasearch available-data "inflation rate" --types metric
datasearch search "Nvidia vs AMD revenue" --effort deep
datasearch answer "How has US CPI moved since 2020?"
datasearch contents "<card webpage_url>" --mode inline --content-format csv
```

`search` and `answer` take the same options. `--data-count 0` gives a web-only
search and `--web-count 0` gives a data-only search, since a source is queried
only when its key is present in the request. `--node-id` (repeatable) pins
graph nodes from `available-data` as retrieval candidates, which is a strong
boost; add `--strict` to return only cards matching the pinned nodes.

Node ids are **not durable** across knowledge-graph rebuilds. Resolve them in
the same session you use them; don't cache them across days.

## Cost

`available-data` and `health` are free. `search` and `answer` are billed per
call, and `--effort deep` costs more than the `fast` default. `contents`
includes a 20-row free allowance per card export and bills per 1,000 rows
beyond it. Pass `--quote-only` to price an export without paying for it.

## Notes for maintainers

`available-data` is a port of the `tako_available_data` tool from the
[tako-mcp](https://github.com/TakoData/tako-mcp) workers: one `graph/search`
(limit 10), then a type-aware `graph/related` drill on the top 2 hits with
per-node error isolation, then a deterministic summary. No LLM call is
involved. The pure selection and formatting logic lives in `_coverage.py`
with unit tests in `tests/test_client.py`, and `client._run_available_data`
orchestrates the fetches. The raw graph primitives remain as private
`TakoClient._graph_search`/`_graph_related` methods, so re-exposing pagination
or cohort walking later is a CLI change rather than a rebuild.

Tests live in `tests/` (no `__init__.py`) rather than at the tool root. The
tool directory shares the SDK's import name, and pytest's module naming would
otherwise load this package as top-level `tako` and shadow the SDK.

The keyless path lives in `_mcp.py`: a small Streamable HTTP JSON-RPC client
(httpx, which honors `HTTPS_PROXY`/`SSL_CERT_FILE` natively) against the
hosted MCP's `tako_search`/`tako_answer`/`tako_available_data` tools, with
widget fields stripped so both backends return the same shape. Routing is by
`_is_configured("TAKO_API_KEY")` (ctx.secrets membership, never the key
value), the same helper websearch uses. No `Mcp-Session-Id` is sent: the
Worker is stateless and never issues one, and free-tier metering is per
client IP (Cloudflare `CF-Connecting-IP`), not per session.

The argument mapping in `_mcp.py` is verified against the hosted worker's
tool schemas (tako-mcp `workers/src/tools/`) and the anonymous-tier contract
(TakoData/tako-mcp#171) as of 2026-07-26: search takes a single `count` plus
`sources[]` and its `effort` enum is fast/instant only; answer takes neither
`effort` nor `count`; `other_matches` come back without `node_id`; the
widget fields are top-level on the tool output; the anonymous surface is
exactly the three tools this backend calls. Every knob mismatch degrades to
a `meta.partial_failures` entry client-side; a disabled tier 401s
(`McpAuthRequired`), an over-limit IP 429s (`McpRateLimited`, surfacing the
server's own message and Retry-After).

Three SDK workarounds worth knowing about, all in `client.py`:

- **Graph auth.** The SDK's OpenAPI spec is missing security declarations on
  the beta graph endpoints, so the generated client attaches no `X-API-Key`
  to `graph_search`/`graph_related` and the API answers 401. The client sets
  the key as a default header on the underlying `ApiClient`, which covers
  every operation. Remove that line once the spec is fixed upstream.

- **Proxy wiring.** `tako-sdk`'s transport is urllib3, which does not read
  proxy settings from the environment. With `Configuration.proxy` unset it
  builds a plain `PoolManager` and dials the upstream directly. The request
  then bypasses iron-proxy entirely: the credential placeholder is never
  swapped, and the sandbox NetworkPolicy drops the connection. The client
  reads `HTTPS_PROXY` and `SSL_CERT_FILE`/`REQUESTS_CA_BUNDLE` explicitly,
  the same shape as `tools/productivity/gsuite/client.py`.
- **Timeouts.** The SDK accepts `_request_timeout` per request, but the `Tako`
  facade doesn't forward it, and urllib3 defaults to no timeout with POST
  excluded from retries, so a hung read would block until the sandbox kills
  the process. The client therefore calls the generated `TakoApi` directly
  (`_client._api`) and passes `_request_timeout` on every operation: 120s for
  search/answer/contents, 30s for graph calls, both overridable via the
  `TakoClient` constructor. Worth an upstream facade fix, at which point the
  `_api` reach-through can go.

The SDK agent products (`client.agent.retrieval`, `client.agent.answer`) are
deliberately not exposed. They are async run/poll/stream, which fits a
workflow better than a one-shot CLI shim.
